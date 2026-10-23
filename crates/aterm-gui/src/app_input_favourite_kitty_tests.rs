// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::{App, WindowId};
use aterm_effects::kitty_registry::{KittyLook, glyph_key};

/// The promoted cat is the launch kitty, and the press presents it through the collection hello.
#[test]
fn favourite_promotes_the_launch_kitty() {
    let mut app = App::headless_for_test();
    app.recompute_sparkle();
    let wid = WindowId(0);
    let session = app
        .front_terminal(wid)
        .expect("headless front terminal")
        .session;
    let look = KittyLook::for_launch(App::TEST_LAUNCH_SEED);
    assert_eq!(
        app.launch_kitty, look,
        "the harness wears the fixed-seed cat"
    );
    let now = std::time::Instant::now();

    let other = KittyLook {
        variant: aterm_effects::cat_glyphs_gen::CatGlyphId::SpecWitch,
        ..KittyLook::default()
    }
    .normalized();
    assert_ne!(other, look, "the fixture must differ from the launch kitty");
    app.kitty_log.observe(
        session,
        [aterm_effects::kitty_registry::KittySighting {
            kitty_type: aterm_effects::kitty_registry::KittyType::HeadPeek,
            magic: aterm_effects::kitty_registry::KittyMagic::None,
            shown_as: aterm_effects::kitty_registry::KittyShownAs::Cat,
            langs: aterm_lexicon::LangSet::EMPTY,
            traits: 0,
            look: other,
            ident: 0xFEED,
        }],
        aterm_lexicon::Lexicon::builtin(),
        now,
        true,
    );
    assert_eq!(
        app.kitty_log.favourite_look(),
        None,
        "a discovery collects but pins nothing — the launch kitty rides"
    );
    assert_eq!(
        app.companion_verdict(wid, session, now),
        look,
        "…and the verdict says so"
    );

    app.favourite_kitty_checked(wid, now)
        .expect("the pin these assertions read");

    assert_eq!(
        app.kitty_log.favourite_look(),
        Some(look),
        "the launch kitty is the one that got promoted"
    );
    assert_eq!(
        app.companion_verdict(wid, session, now),
        look,
        "the verdict now rests on the pin"
    );
    let row = app
        .kitty_log
        .log()
        .collectibles
        .iter()
        .find(|item| item.key == glyph_key(look.variant))
        .expect("the launch kitty's head is now a durable roster row")
        .clone();
    assert!(!row.favourite.is_empty(), "stamped as the user's pick");
    assert_eq!(row.coat, look.coat, "wearing the pinned composition");
    assert_eq!(row.iris, look.iris);

    assert!(app.windows[&wid].fx.companions().cursor_cat.is_active());
    let frame = app
        .windows
        .get_mut(&wid)
        .expect("the window is live")
        .fx
        .companions_mut()
        .cursor_cat
        .static_frame(now);
    assert!(
        frame.collection_hello,
        "a favourite IS a reason to change the identity, so it says hello"
    );
    assert_eq!(frame.look, look, "and the hello wears the promoted cat");
}

/// A declining cat action leaves its refusal in `pending_action_refusal` so `invoke` returns `Err`.
#[test]
fn a_declining_cat_action_produces_a_refusal_rather_than_silence() {
    let wid = WindowId(0);
    let now = std::time::Instant::now();

    let mut app = App::headless_for_test();
    app.config.sparkle_words = Some(crate::app_config::SparkleWordsConfig {
        feline: Some(crate::app_config::SparkleFelineConfig {
            enabled: Some(false),
            ..Default::default()
        }),
        ..Default::default()
    });
    app.prepared_sparkle = app.config.prepare_sparkle_runtime();
    app.sparkle_dirty = true;
    app.recompute_sparkle();

    let pinned = app
        .favourite_kitty_checked(wid, now)
        .expect_err("feline off must refuse, not silently do nothing");
    assert!(pinned.contains("feline"), "{pinned:?}");
    let worn = app
        .wear_next_kitty(wid, now)
        .expect_err("and so must the walk");
    assert!(!worn.is_empty(), "the refusal carries a reason");

    let mut app = App::headless_for_test();
    app.recompute_sparkle();
    let empty = app
        .wear_next_kitty(wid, now)
        .expect_err("nothing collected yet");
    assert!(empty.contains("collected"), "{empty:?}");

    assert!(app.pending_action_refusal.is_none());
}

/// `invoke CloseTab` answers `OK confirm pending …` when it asks, and refuses a second close while pending.
#[test]
fn an_invoke_that_asks_first_answers_ok_pending_and_a_second_is_refused() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let invoke_close_tab = |app: &mut App| {
        app.invoke_attributed(crate::session_store::ExitActor::Ctl, |app| {
            let question_stood = app.close_banner.is_some();
            app.pending_action_refusal = None;
            let prompt =
                crate::quit_safety::confirm_prompt(true, true, true).expect("a busy quit prompts");
            app.present_close_banner(wid, prompt, crate::close_confirm::CloseReplay::Window(wid));
            app.invoke_reply("CloseTab", question_stood)
        })
    };
    assert_eq!(
        invoke_close_tab(&mut app),
        Ok("confirm pending kind=quit window=0; `confirm yes|no` answers it".to_string())
    );
    assert!(
        app.pending_action_refusal.is_none(),
        "the reply consumed the channel"
    );
    assert_eq!(
        invoke_close_tab(&mut app),
        Err(
            "CloseTab: a quit is waiting for an answer in the window; `confirm yes|no` \
             answers it"
                .to_string()
        )
    );
    assert_eq!(app.answer_close_banner(false), None);
    assert_eq!(
        app.invoke_reply("NewTab", false),
        Ok("invoked NewTab".to_string())
    );
}

/// Window and search arms that decline refuse in words, paired with the state in which each acts.
#[test]
fn a_declining_window_or_search_action_refuses_in_words() {
    let mut app = App::headless_for_test();

    let why = app
        .toggle_fullscreen()
        .expect_err("no OS window must refuse, not silently do nothing");
    assert!(why.contains("window"), "{why:?}");

    let why = app
        .migrate_active_tab_to_next_window()
        .expect_err("a single window must refuse");
    assert!(why.contains("other window"), "{why:?}");
    app.open_active_session_in_new_window_logical()
        .expect("share the active session into a second window");
    app.migrate_active_tab_to_next_window()
        .expect("two windows: the move must happen, not refuse");

    let mut app = App::headless_for_test();
    assert!(app.search_last_query.is_empty(), "the harness starts cold");
    for forward in [true, false] {
        let why = app
            .search_find_again(forward)
            .expect_err("nothing to step must refuse");
        assert!(!why.is_empty(), "the refusal carries a reason");
    }
    app.search_last_query = "a".to_string();
    app.search_find_again(true)
        .expect("a remembered query resumes, it does not refuse");
}

/// Wearing a collected cat by key moves the companion verdict to it with the `on_collect` hello.
#[test]
fn wearing_a_collected_cat_by_name_switches_the_companion() {
    use aterm_effects::companion_look::{AppIdentity, TENURE};
    let mut app = App::headless_for_test();
    app.recompute_sparkle();
    let wid = WindowId(0);
    let t0 = std::time::Instant::now();

    app.launch_kitty = KittyLook::for_app("python");
    let launch = app.promotable_kitty(wid);
    app.favourite_kitty_checked(wid, t0)
        .expect("the pin these assertions read");
    assert_eq!(app.kitty_log.favourite_look(), Some(launch));

    let claude = AppIdentity {
        id: "claude".into(),
        basename: "claude".into(),
        look: KittyLook::for_app("claude"),
    };
    {
        let gate = app
            .windows
            .get_mut(&wid)
            .expect("window")
            .fx
            .look_mut()
            .tenure_mut();
        gate.observe(Some(&claude), t0);
        assert!(
            gate.observe(Some(&claude), t0 + TENURE).is_some(),
            "fixture: tenure served"
        );
    }
    app.favourite_kitty_checked(wid, t0 + TENURE)
        .expect("the pin these assertions read");
    assert_eq!(
        app.companion_verdict(wid, 0, t0 + TENURE),
        claude.look,
        "fixture: the program cat is the one worn, and the launch kitty is \
         collected but not on the cursor"
    );
    assert_ne!(
        launch, claude.look,
        "fixture: the two rows are different cats"
    );

    let (worn, rows) = app.kitty_log.wearable();
    assert!(rows.len() >= 2, "two cats collected: {rows:?}");
    let want = rows
        .iter()
        .find(|row| row.look() == Some(launch))
        .expect("the launch kitty is a row in the collection")
        .key
        .clone();
    assert_ne!(
        Some(&want),
        worn.as_ref(),
        "fixture: the cat we are about to wear is NOT the one already on"
    );

    let got = app
        .wear_kitty(wid, &want, t0 + TENURE)
        .expect("a collected cat can be worn");
    assert_eq!(got, launch, "the cat asked for is the cat put on");
    assert_eq!(
        app.companion_verdict(wid, 0, t0 + TENURE),
        launch,
        "and the companion verdict moved to it, over a SERVED program tenure"
    );
    let (worn, _) = app.kitty_log.wearable();
    assert_eq!(worn.as_deref(), Some(want.as_str()), "the list agrees");

    let frame = app
        .windows
        .get_mut(&wid)
        .expect("the window is live")
        .fx
        .companions_mut()
        .cursor_cat
        .static_frame(t0 + TENURE);
    assert!(
        frame.collection_hello,
        "a deliberate pick is a reason to change the identity"
    );

    let before = app.companion_verdict(wid, 0, t0 + TENURE);
    let refused = app
        .wear_kitty(wid, "no-such-cat", t0 + TENURE)
        .expect_err("an uncollected cat cannot be worn");
    assert!(refused.contains("no-such-cat"), "{refused:?}");
    assert_eq!(
        app.companion_verdict(wid, 0, t0 + TENURE),
        before,
        "a refused wear leaves the cat where it was"
    );
}

/// Next Kitty walks the collection and wraps around.
#[test]
fn next_kitty_walks_the_collection_and_comes_back_round() {
    use aterm_effects::companion_look::{AppIdentity, TENURE};
    let mut app = App::headless_for_test();
    app.recompute_sparkle();
    let wid = WindowId(0);
    let t0 = std::time::Instant::now();

    assert!(
        app.wear_next_kitty(wid, t0).is_err(),
        "an empty collection has no next"
    );

    app.launch_kitty = KittyLook::for_app("python");
    let a = app.promotable_kitty(wid);
    app.favourite_kitty_checked(wid, t0)
        .expect("the pin these assertions read");
    let claude = AppIdentity {
        id: "claude".into(),
        basename: "claude".into(),
        look: KittyLook::for_app("claude"),
    };
    {
        let gate = app
            .windows
            .get_mut(&wid)
            .expect("window")
            .fx
            .look_mut()
            .tenure_mut();
        gate.observe(Some(&claude), t0);
        assert!(gate.observe(Some(&claude), t0 + TENURE).is_some());
    }
    app.favourite_kitty_checked(wid, t0 + TENURE)
        .expect("the pin these assertions read");
    let b = claude.look;
    assert_ne!(a, b, "fixture: two different cats");
    assert_eq!(
        app.kitty_log.favourite_look(),
        Some(b),
        "fixture: wearing b"
    );

    let first = app
        .wear_next_kitty(wid, t0 + TENURE)
        .expect("two collected cats have a next");
    assert_eq!(first, a, "next from b is a");

    let second = app
        .wear_next_kitty(wid, t0 + TENURE)
        .expect("and a next again");
    assert_eq!(second, b, "and next from a wraps to b");
    assert_eq!(
        app.companion_verdict(wid, 0, t0 + TENURE),
        b,
        "the cat on the cursor followed both presses"
    );
}

/// The sparkle master and feline sub-gate refuse without leaving a trace, while an unread cache resolves first.
#[test]
fn favourite_respects_the_sparkle_master_and_feline_gates() {
    let wid = WindowId(0);
    let now = std::time::Instant::now();

    let mut app = App::headless_for_test();
    app.config.sparkle_words = Some(crate::app_config::SparkleWordsConfig {
        enabled: Some(false),
        ..Default::default()
    });
    app.prepared_sparkle = app.config.prepare_sparkle_runtime();
    app.sparkle_dirty = true;
    app.recompute_sparkle();
    assert!(
        app.sparkle.is_none(),
        "the master is off, not merely unread"
    );
    let refused = app.favourite_kitty_checked(wid, now);
    assert!(
        refused.is_err(),
        "the gate refuses, and says so: {refused:?}"
    );
    assert_eq!(app.kitty_log.favourite_look(), None, "master off: no pin");
    assert_eq!(app.kitty_log.log().sightings, 0);
    assert!(!app.windows[&wid].fx.companions().cursor_cat.is_active());

    let mut app = App::headless_for_test();
    assert!(
        app.sparkle.is_none() && app.sparkle_dirty,
        "fixture: the cache is unread, exactly as it is at launch"
    );
    app.favourite_kitty_checked(wid, now)
        .expect("the cache is merely unread, not off — the pin lands");
    assert!(
        app.kitty_log.favourite_look().is_some(),
        "a config with cats ON must pin on the first press, drawn frame or not"
    );

    let mut app = App::headless_for_test();
    app.config.sparkle_words = Some(crate::app_config::SparkleWordsConfig {
        feline: Some(crate::app_config::SparkleFelineConfig {
            enabled: Some(false),
            ..Default::default()
        }),
        ..Default::default()
    });
    app.prepared_sparkle = app.config.prepare_sparkle_runtime();
    app.sparkle_dirty = true;
    app.recompute_sparkle();
    assert!(
        !app.sparkle
            .as_ref()
            .expect("the remaining families keep the sparkle master resolved ON")
            .cfg
            .feline
    );
    let refused = app.favourite_kitty_checked(wid, now);
    assert!(
        refused.is_err(),
        "the feline gate refuses, and says so: {refused:?}"
    );
    assert_eq!(app.kitty_log.favourite_look(), None, "feline off: no pin");
    assert_eq!(app.kitty_log.log().sightings, 0);
    assert!(!app.windows[&wid].fx.companions().cursor_cat.is_active());
}

/// Program cats arrive after `TENURE`, linger until `RELEASE`, match the capture splice, and yield to a favourite pin.
#[test]
fn program_cats_arrive_by_tenure_linger_and_the_capture_seam_agrees() {
    use aterm_effects::companion_look::{RELEASE, TENURE};
    let t0 = std::time::Instant::now();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.recompute_sparkle();
    let launch = KittyLook::for_launch(App::TEST_LAUNCH_SEED);
    let claude = KittyLook::for_app("claude");
    assert_ne!(claude, launch, "fixture: distinguishable");
    let term = app.pool.get(0).expect("session 0").term.clone();
    let session = app.front_terminal(wid).expect("front terminal").session;
    let dressed = |app: &mut App, t: std::time::Instant| {
        app.capture_words(wid, t);
        app.windows
            .get_mut(&wid)
            .expect("window")
            .fx
            .companions_mut()
            .cursor_cat
            .static_frame(t)
            .look
    };

    {
        let mut t = crate::term_lock(&term);
        t.process(b"\x1b]133;A\x07user@host repo % \x1b]133;B\x07");
    }
    assert_eq!(app.companion_verdict(wid, session, t0), launch);
    assert_eq!(
        dressed(&mut app, t0),
        launch,
        "at the prompt: the launch kitty"
    );

    let t1 = t0 + std::time::Duration::from_secs(1);
    {
        let mut t = crate::term_lock(&term);
        t.process(b"claude --resume\x1b]633;E;claude --resume\x07\r\n\x1b]133;C\x07");
    }
    assert_eq!(
        app.companion_verdict(wid, session, t1),
        launch,
        "just started: no tenure yet, still the base cat"
    );
    assert_eq!(dressed(&mut app, t1), launch);
    assert_eq!(
        app.windows[&wid].fx.look().tenure().deadline(),
        Some(t1 + TENURE),
        "the wake is armed for the moment claude earns the cursor"
    );
    let t2 = t1 + TENURE;
    assert_eq!(
        app.companion_verdict(wid, session, t2),
        claude,
        "tenure served: the claude cat"
    );
    assert_eq!(
        dressed(&mut app, t2),
        claude,
        "…and the capture seam agrees"
    );

    let t3 = t2 + std::time::Duration::from_secs(60);
    {
        let mut t = crate::term_lock(&term);
        t.process(b"\x1b]133;D;0\x07\x1b]133;A\x07user@host repo % \x1b]133;B\x07");
    }
    assert_eq!(app.companion_verdict(wid, session, t3), claude, "lingering");
    assert_eq!(dressed(&mut app, t3), claude);
    {
        let mut t = crate::term_lock(&term);
        t.process(b"ls\x1b]633;E;ls\x07\r\n\x1b]133;C\x07");
    }
    let t4 = t3 + std::time::Duration::from_millis(300);
    assert_eq!(
        app.companion_verdict(wid, session, t4),
        claude,
        "`ls` earns nothing"
    );
    {
        let mut t = crate::term_lock(&term);
        t.process(b"\x1b]133;D;0\x07\x1b]133;A\x07user@host repo % \x1b]133;B\x07");
    }
    let t5 = t4 + std::time::Duration::from_millis(200);
    assert_eq!(
        app.companion_verdict(wid, session, t5),
        claude,
        "home: still lingering"
    );
    let t6 = t5 + RELEASE;
    assert_eq!(
        app.companion_verdict(wid, session, t6),
        launch,
        "settled at the prompt: the launch kitty is back"
    );
    assert_eq!(dressed(&mut app, t6), launch);

    let pinned = KittyLook {
        variant: aterm_effects::cat_glyphs_gen::CatGlyphId::S101,
        coat: (launch.coat + 5) % 16,
        ..KittyLook::default()
    }
    .normalized();
    assert!(
        pinned != launch && pinned != claude,
        "fixture: distinguishable"
    );
    app.kitty_log.favourite(
        &aterm_effects::kitty_registry::KittySighting {
            kitty_type: aterm_effects::kitty_registry::KittyType::HeadPeek,
            magic: aterm_effects::kitty_registry::KittyMagic::None,
            shown_as: aterm_effects::kitty_registry::KittyShownAs::Cat,
            langs: aterm_lexicon::LangSet::EMPTY,
            traits: 0,
            look: pinned,
            ident: 0xF00D,
        },
        aterm_lexicon::Lexicon::builtin(),
        t6,
        true,
    );
    {
        let mut t = crate::term_lock(&term);
        t.process(b"claude\x1b]633;E;claude\x07\r\n\x1b]133;C\x07");
    }
    let t7 = t6 + TENURE + std::time::Duration::from_secs(1);
    assert_eq!(
        app.companion_verdict(wid, session, t7),
        pinned,
        "the pin outranks a tenured program cat"
    );
    assert_eq!(
        dressed(&mut app, t7),
        pinned,
        "the capture seam wears the pin"
    );
}

/// The `FavouriteKitty` menu row is reachable by `invoke` and checks once pinned.
#[test]
fn the_favourite_row_is_invoke_reachable_and_checks_once_pinned() {
    let mut app = App::headless_for_test();
    app.recompute_sparkle();
    let wid = WindowId(0);

    let before = app.palette_snapshot(wid);
    assert_eq!(
        before.action_by_name("FavouriteKitty"),
        Ok(crate::menu::MenuAction::FavouriteKitty),
        "the row must be reachable by `aterm-ctl invoke`"
    );
    let row = |state: &crate::palette::PaletteState| {
        state
            .controls_lines()
            .into_iter()
            .find(|line| line.contains("action=FavouriteKitty"))
            .expect("the View menu contributes the row")
    };
    let listed = row(&before);
    assert!(listed.contains("enabled=true"), "{listed}");
    assert!(
        listed.contains("checked=false"),
        "nothing pinned yet: {listed}"
    );
    assert!(
        !app.palette_live().kitty_favourited,
        "and the live predicate agrees"
    );

    app.favourite_kitty_checked(wid, std::time::Instant::now())
        .expect("the pin these assertions read");

    let listed = row(&app.palette_snapshot(wid));
    assert!(
        listed.contains("checked=true"),
        "the row reports the launch kitty as the pin: {listed}"
    );
    assert!(app.palette_live().kitty_favourited);
}
