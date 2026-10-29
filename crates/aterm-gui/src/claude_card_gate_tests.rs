// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the Claude card decoration gate and re-show hold window.

use super::*;

use aterm_core::terminal::HolderView;
use aterm_grid::{FenceKnob, with_knob};

use crate::holder_image::{CardGate, GateBuild, HolderImage};

/// A build `version` names.
fn build(version: &str) -> atpkg::vendor_direct::Version {
    atpkg::vendor_direct::Version::parse(version).expect("a vendor build")
}

/// The card probe's dev rows of `kind` on `session`'s timeline, oldest first
/// (`SessionTimeline::note_card_frame`).
fn dev_rows(app: &App, session: u64, kind: &str) -> Vec<String> {
    app.pool
        .get(session)
        .expect("session")
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .since(None)
        .filter(|e| e.kind == kind)
        .map(|e| e.payload.to_string())
        .collect()
}

/// The reasons the `card-bare` rows gave, in order.
fn bare_reasons(app: &App, session: u64) -> Vec<String> {
    dev_rows(app, session, "card-bare")
        .iter()
        .filter_map(|row| {
            row.split(' ')
                .find_map(|w| w.strip_prefix("reason="))
                .map(str::to_string)
        })
        .collect()
}

/// A headless window 120 wide, its front session supervised, the dev card
/// probe on: nothing on its screen and nothing in front yet.
fn bare_window() -> (App, WindowId, u64, crate::harness_host::HostHandle) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    assert!(app.apply_term_resize(wid, 24, 120));
    app.session_factory.card_probe = true;
    let host = supervise(&mut app, session);
    (app, wid, session, host)
}

/// Bytes into `session`'s engine, as its PTY would carry them.
fn bytes(app: &App, session: u64, s: &str) {
    let term = app.pool.get(session).expect("session").term.clone();
    crate::term_lock(&term).process(s.as_bytes());
}

/// Claude Code's launch card and prompt as it paints them inline (no clear), from the cursor down:
/// a blank row, the card, a blank row, the composer between its rules and the mode row.
fn inline_claude(version: &str) -> String {
    let screen = card_screen(24, 120, MODEL, version);
    let rule = "\u{2500}".repeat(120);
    let rows = [
        String::new(),
        screen[1].clone(),
        screen[2].clone(),
        screen[3].clone(),
        String::new(),
        rule.clone(),
        "\u{276F} ".to_string(),
        rule,
        "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)".to_string(),
    ];
    rows.join("\r\n")
}

/// `rows` written at their own rows from the top with no clear (cursor addressing and erase-line
/// alone, the path row in [`GRAY`] as [`feed`] paints it): what a program that does not clear the
/// screen leaves.
fn paint_at(app: &App, session: u64, rows: &[String]) {
    let mut out = String::new();
    for (i, row) in rows.iter().enumerate() {
        let sgr = if row.ends_with("~/aterm") {
            format!("\x1b[38;2;{};{};{}m", GRAY[0], GRAY[1], GRAY[2])
        } else {
            String::new()
        };
        out.push_str(&format!("\x1b[{};1H\x1b[2K{sgr}{row}\x1b[0m", i + 1));
    }
    bytes(app, session, &out);
}

/// A shell's `cat card.txt`: Claude Code's card and prompt printed by
/// another program, the shell's next prompt under it, the cursor on the
/// blank row below.
fn cat_card(app: &App, session: u64) {
    bytes(app, session, "$ cat card.txt\r\n");
    bytes(app, session, &inline_claude(VERSION));
    bytes(app, session, "\r\n$ claude\r\n");
}

/// The gate checks token, reader kind, fence, and build policy, failing closed.
#[test]
fn the_gate_checks_token_kind_fence_and_build_and_fails_closed() {
    let rows = card_screen(24, 120, MODEL, VERSION);
    let read = read_card(1, &rows, None).read.expect("the card");
    const T: u64 = 0x5eed_0001;
    let gate = CardGate {
        token: T,
        kind: 3,
        rows: 3,
        build: GateBuild::Exact(build(VERSION)),
    };
    let view = |spans: &[(u64, u64)], tainted: bool, alt_fresh: bool| {
        let mut holder = HolderView {
            token: T,
            tainted,
            alt_fresh,
            ..HolderView::NONE
        };
        for (i, span) in spans.iter().enumerate() {
            holder.spans[i] = *span;
        }
        holder.n = u8::try_from(spans.len()).expect("a few spans");
        holder
    };
    let main = |holder: HolderView| FenceFrame {
        holder,
        base_y: 100,
        alt: false,
        mutations: 0,
    };
    let all = view(&[(100, u64::MAX)], false, false);
    assert_eq!(
        admit(&gate, &main(all), &read),
        Ok(()),
        "the drawer's own card"
    );
    let no_token = CardGate { token: 0, ..gate };
    assert_eq!(admit(&no_token, &main(all), &read), Err(Bare::Token));
    let other = HolderView {
        token: T + 1,
        ..all
    };
    assert_eq!(admit(&gate, &main(other), &read), Err(Bare::Token));
    let agy = CardGate { kind: 4, ..gate };
    assert_eq!(admit(&agy, &main(all), &read), Err(Bare::Kind));
    let top = 100 + read.place.top as u64;
    let fence = |spans: &[(u64, u64)]| admit(&gate, &main(view(spans, false, false)), &read);
    assert_eq!(
        admit(&gate, &main(view(&[(100, u64::MAX)], true, false)), &read),
        Err(Bare::Tainted)
    );
    assert_eq!(fence(&[(top, top + 3)]), Ok(()), "exactly its rows");
    assert_eq!(
        fence(&[(top + 1, u64::MAX)]),
        Err(Bare::Fence),
        "the banner outside"
    );
    assert_eq!(
        fence(&[(top, top + 2)]),
        Err(Bare::Fence),
        "its last row outside"
    );
    assert_eq!(
        fence(&[(top, top + 1), (top + 1, u64::MAX)]),
        Err(Bare::Fence),
        "two spans never join: one must hold every row"
    );
    assert_eq!(fence(&[]), Err(Bare::Fence), "no span");
    let above = FenceFrame {
        base_y: -1,
        ..main(all)
    };
    assert_eq!(
        admit(&gate, &above, &read),
        Err(Bare::Fence),
        "no row to name"
    );
    let alt = |holder| FenceFrame {
        holder,
        base_y: 100,
        alt: true,
        mutations: 0,
    };
    assert_eq!(admit(&gate, &alt(all), &read), Err(Bare::Fence));
    assert_eq!(
        admit(&gate, &alt(view(&[], true, true)), &read),
        Ok(()),
        "a fresh alternate screen is wholly the drawer's"
    );
    let with = |policy| CardGate {
        build: policy,
        ..gate
    };
    let next = build("2.1.303");
    assert_eq!(
        admit(&with(GateBuild::Exact(next)), &main(all), &read),
        Err(Bare::Build),
        "a 2.1.303 image under a 2.1.283 banner"
    );
    assert_eq!(
        admit(
            &with(GateBuild::Registry(build(VERSION))),
            &main(all),
            &read
        ),
        Ok(())
    );
    assert_eq!(
        admit(&with(GateBuild::Registry(next)), &main(all), &read),
        Err(Bare::Build)
    );
    assert_eq!(
        admit(&with(GateBuild::ProvenanceOnly), &main(all), &read),
        Ok(()),
        "the fence alone"
    );
    assert_eq!(
        admit(&with(GateBuild::None), &main(all), &read),
        Err(Bare::Build),
        "nothing to hold the banner to"
    );
}

/// Build policy by image source and program banner build.
#[test]
fn the_build_policy_is_the_images_source_by_its_banner_build() {
    use atpkg::image::{ImageBuild, ImageSource, ProgramTag};
    let image = |program: &str, version: Option<&str>, source| {
        HolderImage::of(
            PGID,
            Some(1),
            Some(std::path::PathBuf::from(format!("/bin/{program}"))),
            ImageBuild {
                program: ProgramTag::new(program),
                build: version.map_or(0, |v| build(v).build_id()),
                source,
            },
            1,
        )
    };
    let row = Some(build("2.1.290"));
    for source in [
        ImageSource::Store,
        ImageSource::StoreRecord,
        ImageSource::Native,
    ] {
        assert_eq!(
            image("claude", Some(VERSION), source).gate_build(|| row),
            Some(GateBuild::Exact(build(VERSION))),
            "{source:?}: its own build, never the row's"
        );
    }
    let unknown = image("claude", None, ImageSource::Unknown);
    assert_eq!(
        unknown.gate_build(|| row),
        Some(GateBuild::Registry(build("2.1.290")))
    );
    assert_eq!(unknown.gate_build(|| None), Some(GateBuild::None));
    for program in ["agy", "muse"] {
        for source in [ImageSource::Store, ImageSource::Unknown] {
            assert_eq!(
                image(program, Some("1.4.2"), source).gate_build(|| row),
                Some(GateBuild::ProvenanceOnly),
                "{program} {source:?}"
            );
        }
    }
    assert_eq!(
        image("zsh", None, ImageSource::Unknown).gate_build(|| row),
        None
    );
    assert_eq!(
        image("claude", Some(VERSION), ImageSource::Launcher).gate(|| row),
        None,
        "a launcher draws no card"
    );
    let mut timeline = crate::session_timeline::SessionTimeline::default();
    let claude = claude_image(PGID, VERSION, 1);
    let token = claude.token.expect("a token");
    timeline.offer_holder_image(Arc::new(claude), crate::holder_image::EdgeKind::Group);
    timeline.note_foreground_group(PGID + 1);
    assert_eq!(
        timeline.card_gate(),
        None,
        "CONTROL: another group in front"
    );
    timeline.note_foreground_group(PGID);
    assert_eq!(
        timeline.card_gate(),
        Some(CardGate {
            token,
            kind: 3,
            rows: 3,
            build: GateBuild::Exact(build(VERSION)),
        })
    );
}

/// A Claude Code with a published image and cut fence is decorated on its first frame with no
/// footer facts.
#[test]
fn a_drawer_with_no_facts_is_decorated_on_its_first_frame() {
    let (mut app, wid, session, host) = bare_window();
    launch(&app, session, VERSION, PGID, 1);
    feed(&app, session, &card_screen(24, 120, MODEL, VERSION));
    let t0 = Instant::now();
    set_now(Some(t0));
    frame(&mut app, wid);
    assert!(
        decorated(&app, wid).is_some(),
        "decorated on its first frame"
    );
    let facts = app
        .pool
        .get(session)
        .unwrap()
        .ctx
        .timeline
        .lock()
        .unwrap()
        .claude_footer()
        .is_some();
    assert!(!facts, "no facts were published");
    let decorated_rows = dev_rows(&app, session, "card-decorated");
    assert_eq!(decorated_rows.len(), 1, "{decorated_rows:?}");
    assert!(
        decorated_rows[0].contains("bare_frames=0"),
        "{decorated_rows:?}"
    );
    set_now(Some(t0 + Duration::from_millis(FRAME_MS)));
    frame(&mut app, wid);
    set_now(Some(t0 + Duration::from_millis(EPISODE_MS + 1)));
    frame(&mut app, wid);
    hold(&mut app, wid);
    for i in 0..6u64 {
        set_now(Some(t0 + Duration::from_millis(EPISODE_MS + 100 + i)));
        let probe = i < 3;
        app.session_factory.card_probe = probe;
        let cost = pass_cost(&mut app, wid);
        assert_eq!(
            (
                cost.timeline_locks,
                cost.probe_locks,
                cost.clock_reads,
                cost.allocations,
                cost.ticks
            ),
            (1, usize::from(probe), 0, 0, 0),
            "pass {i} (probe {probe}): one lock, the probe's own, no clock, no allocation: \
             {cost:?}"
        );
    }
    app.session_factory.card_probe = true;
    assert!(decorated(&app, wid).is_some(), "still decorated");
    app.pool
        .get(session)
        .unwrap()
        .ctx
        .timeline
        .lock()
        .unwrap()
        .note_foreground_group(PGID + 9);
    let cost = pass_cost(&mut app, wid);
    assert_eq!(cost.timeline_locks, 1, "CONTROL: one lock: {cost:?}");
    assert!(
        decorated(&app, wid).is_none(),
        "CONTROL: not current, no card"
    );
    set_now(None);
    host.shutdown_and_join();
}

/// The gate keeps every card bare unless its token, reader kind, build, untainted mark, and fence
/// match the drawer.
#[test]
fn the_gate_keeps_every_card_but_its_drawers_own_bare() {
    let screen = card_screen(24, 120, MODEL, VERSION);
    let run = |setup: &dyn Fn(&App, u64)| {
        let (mut app, wid, session, host) = bare_window();
        setup(&app, session);
        frame(&mut app, wid);
        let place = decorated(&app, wid);
        let reasons = bare_reasons(&app, session);
        host.shutdown_and_join();
        (place, reasons)
    };
    let (own, _) = run(&|app, session| {
        launch(app, session, VERSION, PGID, 1);
        feed(app, session, &screen);
    });
    assert!(own.is_some(), "CONTROL: the drawer's own card decorates");
    let (place, reasons) = run(&|app, session| {
        launch(app, session, "2.1.303", PGID, 1);
        feed(app, session, &screen);
    });
    assert_eq!(
        (place, reasons.last().map(String::as_str)),
        (None, Some("build"))
    );
    let (place, reasons) = run(&|app, session| {
        let token = launch(app, session, VERSION, PGID, 1);
        cut(app, session, Some(token ^ 1), CutKind::Exec, false);
        feed(app, session, &screen);
    });
    assert_eq!(
        (place, reasons.last().map(String::as_str)),
        (None, Some("token"))
    );
    let agy = agy_card_screen("1.4.0");
    let (place, reasons) = run(&|app, session| {
        launch(app, session, VERSION, PGID, 1);
        feed(app, session, &agy);
    });
    assert_eq!(
        (place, reasons.last().map(String::as_str)),
        (None, Some("kind"))
    );
    let tainted = |app: &App, session: u64| {
        paint_at(app, session, &screen);
        bytes(app, session, "\x1b[H");
        launch(app, session, VERSION, PGID, 1);
        bytes(app, session, "\x1b[24;1H");
    };
    let (place, reasons) = run(&tainted);
    assert_eq!(
        (place, reasons.last().map(String::as_str)),
        (None, Some("tainted"))
    );
    let (place, _) = with_knob(FenceKnob::NoLayTaint, || run(&tainted));
    assert!(
        place.is_some(),
        "CONTROL: untainted, the same card decorates"
    );
    let (place, reasons) = run(&|app, session| {
        cat_card(app, session);
        launch(app, session, VERSION, PGID, 1);
    });
    assert_eq!(
        (place, reasons.last().map(String::as_str)),
        (None, Some("fence"))
    );
    let (place, _) = run(&|app, session| {
        bytes(app, session, "$ claude\r\n");
        launch(app, session, VERSION, PGID, 1);
        bytes(app, session, &inline_claude(VERSION));
    });
    assert!(place.is_some(), "CONTROL: painted after the cut, decorated");
}

/// agy's launch card as its 1.4.0 fixtures draw it, under `version`, on a
/// screen with nothing else: rows 4 to 8.
fn agy_card_screen(version: &str) -> Vec<String> {
    let mut screen = vec![String::new(); 4];
    screen.extend([
        format!("      \u{2584}\u{2580}\u{2580}\u{2584}        Antigravity CLI {version}"),
        "     \u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}       redacted-user@example.test (Google AI Ultra)".to_string(),
        "    \u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}      Gemini 4 Argon (Medium)".to_string(),
        "   \u{2584}\u{2580}\u{2580}    \u{2580}\u{2580}\u{2584}     ~/aterm".to_string(),
        "  \u{2584}\u{2580}\u{2580}      \u{2580}\u{2580}\u{2584}".to_string(),
    ]);
    while screen.len() < 24 {
        screen.push(String::new());
    }
    screen
}

/// The image of an agy of build `version` leading group `pgid`: a store image, whose banner the
/// gate does not hold to its build.
fn agy_image(pgid: i32, version: &str) -> HolderImage {
    use atpkg::image::{ImageBuild, ImageSource, ProgramTag};
    HolderImage::of(
        pgid,
        Some(3),
        Some(std::path::PathBuf::from("/p/pkg/store/agy/bin/agy")),
        ImageBuild {
            program: ProgramTag::new("agy"),
            build: build(version).build_id(),
            source: ImageSource::Store,
        },
        1,
    )
}

/// An agy card is judged by its fence alone (`ProvenanceOnly`), keeping cards above its span bare.
#[test]
fn an_agy_card_is_judged_by_its_fence_alone() {
    let agy = agy_card_screen("1.4.0");
    let run = |before: bool| {
        let (mut app, wid, session, host) = bare_window();
        if before {
            feed(&app, session, &agy);
            bytes(&app, session, "\x1b[24;1H\r\n");
        }
        let image = agy_image(PGID, "1.4.2");
        let token = image.token.expect("a token");
        {
            let entry = app.pool.get(session).unwrap();
            let mut timeline = entry.ctx.timeline.lock().unwrap();
            timeline.note_foreground_group(PGID);
            timeline.offer_holder_image(Arc::new(image), crate::holder_image::EdgeKind::Group);
            assert_eq!(
                timeline.card_gate().map(|g| g.build),
                Some(GateBuild::ProvenanceOnly)
            );
        }
        cut(&app, session, Some(token), CutKind::Group, false);
        if !before {
            feed(&app, session, &agy);
        }
        frame(&mut app, wid);
        let place = decorated(&app, wid);
        let reasons = bare_reasons(&app, session);
        let read = app.windows[&wid]
            .claude_footer_texts
            .borrow()
            .launch_card(0, session)
            .map(|(read, _)| read);
        host.shutdown_and_join();
        (place, reasons, read)
    };
    let (own, _, read) = run(false);
    assert!(
        own.is_some(),
        "its own card, a 1.4.0 banner on a 1.4.2 image"
    );
    assert_eq!(read.map(|r| r.kind), Some(4));
    let (quoted, reasons, read) = run(true);
    assert_eq!(quoted, None, "a predecessor's card above its span");
    assert_eq!(reasons.last().map(String::as_str), Some("fence"));
    let read = read.expect("CONTROL: the quoted card is read");
    let everywhere = FenceFrame {
        holder: HolderView {
            token: 7,
            n: 1,
            spans: [(0, u64::MAX), (0, 0), (0, 0), (0, 0)],
            tainted: false,
            alt_fresh: false,
        },
        base_y: 0,
        alt: false,
        mutations: 0,
    };
    let gate = CardGate {
        token: 7,
        kind: 4,
        rows: aterm_phase::agy::LAUNCH_CARD_ROWS,
        build: GateBuild::ProvenanceOnly,
    };
    assert_eq!(
        admit(&gate, &everywhere, &read),
        Ok(()),
        "CONTROL: with its rows admitted, the build check passes the quoted card"
    );
    let own_banner = CardGate {
        build: GateBuild::Exact(read.build.expect("its banner's build")),
        ..gate
    };
    assert_eq!(
        admit(&own_banner, &everywhere, &read),
        Ok(()),
        "CONTROL: the deleted self-compare passes it too"
    );
    assert_eq!(
        (read.place.rows, aterm_phase::agy::LAUNCH_CARD_ROWS),
        (4, 5),
        "the reader places four rows of a five-row card"
    );
    let span = |rows: u64| {
        let top = read.place.top as u64;
        FenceFrame {
            holder: HolderView {
                token: 7,
                n: 1,
                spans: [(top, top + rows), (0, 0), (0, 0), (0, 0)],
                tainted: false,
                alt_fresh: false,
            },
            base_y: 0,
            alt: false,
            mutations: 0,
        }
    };
    assert_eq!(
        admit(&gate, &span(5), &read),
        Ok(()),
        "every row, the feet too"
    );
    assert_eq!(
        admit(&gate, &span(4), &read),
        Err(Bare::Fence),
        "the rows it places, its feet outside: not the drawer's card"
    );
}

/// Muse Code 1.4.12's first output after its launcher's `\e[3J\e[H\e[2J`, captured from `lab muse`
/// (`ctl subscribe @sid bytes`): four newlines, three reverse indexes at the top of a region (rows
/// 1-7) its span starts on, its banner in the rows they opened, then its composer.
const MUSE_1_4_12_STARTUP: &[u8] = include_bytes!("fixtures/muse-1.4.12-inline-startup.bin");

/// A Muse Code image leading group `pgid`: a vendor binary outside the
/// store, as `lab muse` runs it.
fn muse_image(pgid: i32) -> HolderImage {
    use atpkg::image::{ImageBuild, ProgramTag};
    HolderImage::of(
        pgid,
        Some(5),
        Some(std::path::PathBuf::from("/x/muse-bin-1.4.12-R6909.1")),
        ImageBuild::unknown(ProgramTag::new("muse-bin-1.4.12-R6909.1")),
        1,
    )
}

/// Muse Code's banner in rows it opened (or erased) at its own start is decorated; a previous one's
/// stays bare.
#[test]
fn a_muse_codes_banner_in_the_rows_it_opened_is_decorated() {
    let run = |stale: bool| {
        let (mut app, wid, session, host) = bare_window();
        assert!(app.apply_term_resize(wid, 50, 144));
        bytes(&app, session, "$ muse\r\n\x1b[3J\x1b[H\x1b[2J");
        if stale {
            bytes(
                &app,
                session,
                std::str::from_utf8(MUSE_1_4_12_STARTUP).unwrap(),
            );
            bytes(&app, session, "\x1b[20;1H$ muse\r\n");
        }
        let token = take_front(&app, session, PGID, muse_image(PGID), CutKind::Exec);
        assert_eq!(
            gate_of(&app, session).map(|g| (g.token, g.kind, g.build)),
            Some((token, 1, GateBuild::ProvenanceOnly))
        );
        if !stale {
            bytes(
                &app,
                session,
                std::str::from_utf8(MUSE_1_4_12_STARTUP).unwrap(),
            );
        }
        frame(&mut app, wid);
        let place = decorated(&app, wid);
        let reasons = bare_reasons(&app, session);
        host.shutdown_and_join();
        (place, reasons)
    };
    let (own, reasons) = run(false);
    assert_eq!(own.map(|p| p.top), Some(1), "{reasons:?}");
    let (stale, reasons) = run(true);
    assert_eq!(stale, None, "a previous Muse Code's banner");
    assert_eq!(reasons.last().map(String::as_str), Some("fence"));
    let (followed, reasons) = with_knob(FenceKnob::StartFollows, || run(false));
    assert_eq!(
        followed.map(|p| p.top),
        Some(1),
        "the erase law alone: {reasons:?}"
    );
    let (held, reasons) = with_knob(FenceKnob::ErasedRowsStayOut, || run(false));
    assert_eq!(
        held.map(|p| p.top),
        Some(1),
        "the start that holds alone: {reasons:?}"
    );
    let (neither, reasons) = with_knob(FenceKnob::StartFollows, || {
        with_knob(FenceKnob::ErasedRowsStayOut, || run(false))
    });
    assert_eq!(
        neither, None,
        "CONTROL: the start followed its row down and the erased rows stayed out"
    );
    assert_eq!(reasons.last().map(String::as_str), Some("fence"));
}

/// A same-build inline relaunch keeps the old card above the new process's span bare until it
/// paints its own.
#[test]
fn a_same_build_inline_relaunch_keeps_the_old_card_bare() {
    let (mut app, wid, session, host) = bare_window();
    let t0 = Instant::now();
    set_now(Some(t0));
    bytes(&app, session, "$ claude\r\n");
    let first = launch(&app, session, VERSION, PGID, 1);
    bytes(&app, session, &inline_claude(VERSION));
    frame(&mut app, wid);
    let old = decorated(&app, wid).expect("the first card");
    bytes(&app, session, "\r\n");
    leave(&app, session, false);
    bytes(&app, session, "$ claude\r\n");
    let second = launch(&app, session, VERSION, PGID + 1, 2);
    assert_ne!(first, second, "a new snapshot token");
    set_now(Some(t0 + Duration::from_secs(2)));
    frame(&mut app, wid);
    assert_eq!(decorated(&app, wid), None, "the old card stays bare");
    assert_eq!(
        bare_reasons(&app, session).last().map(String::as_str),
        Some("fence")
    );
    let armed = || dev_rows(&app, session, "card-armed");
    assert_eq!(armed().len(), 1, "nothing armed for the old card");
    bytes(&app, session, &inline_claude(VERSION));
    set_now(Some(t0 + Duration::from_secs(3)));
    frame(&mut app, wid);
    let new = decorated(&app, wid).expect("its own card");
    assert!(
        new.top > old.top,
        "the new card, below the old: {new:?} {old:?}"
    );
    let armed = dev_rows(&app, session, "card-armed");
    assert_eq!(armed.len(), 2, "{armed:?}");
    assert!(armed[1].contains("cause=new"), "{armed:?}");
    set_now(None);
    host.shutdown_and_join();
}

/// Claude Code 2.1.304's first inline frame at 144 columns (`CLAUDE_CODE_NO_FLICKER=0`).
const CLAUDE_2_1_304_INLINE: &str =
    include_str!("../tests/fixtures/claude-card/claude-2.1.304-inline-first-frame-144col.out");

/// A same-build inline relaunch on Claude Code 2.1.304's captured first-frame bytes keeps the old
/// card bare.
#[test]
fn a_relaunch_of_2_1_304_keeps_its_old_card_bare_on_its_own_bytes() {
    const BUILD: &str = "2.1.304";
    let (mut app, wid, session, host) = bare_window();
    assert!(app.apply_term_resize(wid, 50, 144));
    let t0 = Instant::now();
    set_now(Some(t0));
    bytes(&app, session, "$ claude\r\n");
    let first = launch(&app, session, BUILD, PGID, 1);
    bytes(&app, session, CLAUDE_2_1_304_INLINE);
    frame(&mut app, wid);
    let old = decorated(&app, wid).expect("2.1.304's own card, on its first frame");
    bytes(&app, session, "\x1b[3B\r\n");
    leave(&app, session, false);
    bytes(&app, session, "$ claude\r\n");
    let second = launch(&app, session, BUILD, PGID + 1, 2);
    assert_ne!(first, second, "a new snapshot token");
    set_now(Some(t0 + Duration::from_secs(2)));
    frame(&mut app, wid);
    assert_eq!(decorated(&app, wid), None, "the old card stays bare");
    assert_eq!(
        bare_reasons(&app, session).last().map(String::as_str),
        Some("fence")
    );
    assert_eq!(
        dev_rows(&app, session, "card-armed").len(),
        1,
        "nothing armed for it"
    );
    bytes(&app, session, CLAUDE_2_1_304_INLINE);
    set_now(Some(t0 + Duration::from_secs(3)));
    frame(&mut app, wid);
    let new = decorated(&app, wid).expect("the relaunch's own card, on its first frame");
    assert!(new.top > old.top, "under the old one: {new:?} {old:?}");
    let decorated_rows = dev_rows(&app, session, "card-decorated");
    assert_eq!(decorated_rows.len(), 2, "{decorated_rows:?}");
    assert!(
        decorated_rows[1].contains("bare_frames=0"),
        "{decorated_rows:?}"
    );
    let armed = dev_rows(&app, session, "card-armed");
    assert_eq!(armed.len(), 2, "{armed:?}");
    assert!(armed[1].contains("cause=new"), "{armed:?}");
    set_now(None);
    host.shutdown_and_join();
}

/// The alternate screen is the drawer's only once cleared (`1049h` vs `47h`).
#[test]
fn the_alternate_screen_is_the_drawers_only_once_cleared() {
    let screen = card_screen(24, 120, MODEL, VERSION);
    let run = |enter: &str| {
        let (mut app, wid, session, host) = bare_window();
        cat_card(&app, session);
        launch(&app, session, VERSION, PGID, 1);
        frame(&mut app, wid);
        let main = decorated(&app, wid);
        bytes(&app, session, enter);
        paint_at(&app, session, &screen);
        frame(&mut app, wid);
        let alt = decorated(&app, wid);
        let reasons = bare_reasons(&app, session);
        host.shutdown_and_join();
        (main, alt, reasons)
    };
    let (main, alt, _) = run("\x1b[?1049h");
    assert_eq!(main, None, "the main card outside the span is bare");
    assert!(alt.is_some(), "the alt card after 1049h is decorated");
    let (_, alt, reasons) = run("\x1b[?47h");
    assert_eq!(alt, None, "CONTROL: 47h leaves it bare");
    assert_eq!(reasons.last().map(String::as_str), Some("fence"));
}

/// `claude` typed at the shell's prompt: Claude Code (group `pgid`,
/// started at `start`) takes the front and paints its card and prompt
/// inline, under the command line.
fn claude_inline(app: &App, session: u64, pgid: i32, start: u64) -> u64 {
    bytes(app, session, "$ claude\r\n");
    let token = launch(app, session, VERSION, pgid, start);
    bytes(app, session, &inline_claude(VERSION));
    token
}

/// Ctrl-Z: Claude Code leaves its cursor under its prompt and stops; the
/// shell, in front, says so.
fn suspend(app: &App, session: u64) {
    bytes(app, session, "\r\n");
    leave(app, session, true);
    bytes(app, session, "zsh: suspended  claude\r\n");
}

/// `fg`: the stopped Claude Code (`token`, group [`PGID`]) back in front.
fn resume(app: &App, session: u64, token: u64) {
    bytes(app, session, "$ fg\r\n");
    {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.note_foreground_group(PGID);
        timeline.offer_holder_image(
            Arc::new(claude_image(PGID, VERSION, 1)),
            crate::holder_image::EdgeKind::Group,
        );
    }
    cut(app, session, Some(token), CutKind::Group, false);
}

/// A card printed by the shell while Claude Code was stopped stays bare when Claude resumes (`fg`).
#[test]
fn a_card_catted_while_claude_was_stopped_stays_bare() {
    let run = |rewrite: bool, seal: bool| {
        let (mut app, wid, session, host) = bare_window();
        let t0 = Instant::now();
        set_now(Some(t0));
        let token = claude_inline(&app, session, PGID, 1);
        frame(&mut app, wid);
        let own = decorated(&app, wid).expect("decorated");
        suspend(&app, session);
        set_now(Some(t0 + Duration::from_secs(1)));
        frame(&mut app, wid);
        let stopped = decorated(&app, wid);
        if rewrite {
            bytes(&app, session, "$ clear\r\n\x1b[H\x1b[2J$ cat card.txt\r\n");
            let card = inline_claude(VERSION);
            bytes(
                &app,
                session,
                card.strip_prefix("\r\n").expect("a blank row first"),
            );
            bytes(&app, session, "\r\n");
        }
        if seal {
            resume(&app, session, token);
        } else {
            with_knob(FenceKnob::NoSealCheck, || resume(&app, session, token));
        }
        set_now(Some(t0 + Duration::from_secs(2)));
        frame(&mut app, wid);
        let after = decorated(&app, wid);
        let reasons = bare_reasons(&app, session);
        let armed = dev_rows(&app, session, "card-armed").len();
        set_now(None);
        host.shutdown_and_join();
        (own, stopped, after, reasons, armed)
    };
    let (own, stopped, catted, reasons, armed) = run(true, true);
    assert_eq!(stopped, None, "bare while stopped");
    assert_eq!(catted, None, "the cat'd card stays bare");
    assert_eq!(reasons.last().map(String::as_str), Some("fence"));
    assert_eq!(armed, 1, "nothing armed again");
    let (_, _, unsealed, _, armed) = run(true, false);
    let unsealed = unsealed.expect("CONTROL: without the seal check it decorates");
    assert!(
        unsealed.top < own.top,
        "CONTROL: the cat'd card: {unsealed:?}"
    );
    assert_eq!(armed, 1, "CONTROL: the same drawer arms nothing");
    let (own, _, back, _, armed) = run(false, true);
    assert_eq!(
        back,
        Some(own),
        "CONTROL: fg over untouched rows shows its card"
    );
    assert_eq!(armed, 1, "CONTROL: unarmed");
}

/// A card repainted by Claude Code after `fg` is decorated under the shell-printed card.
#[test]
fn a_card_claude_repaints_after_fg_is_decorated_under_the_catted_one() {
    let (mut app, wid, session, host) = bare_window();
    let t0 = Instant::now();
    set_now(Some(t0));
    let token = claude_inline(&app, session, PGID, 1);
    frame(&mut app, wid);
    let first = dev_rows(&app, session, "card-decorated");
    assert_eq!(first.len(), 1, "{first:?}");
    suspend(&app, session);
    set_now(Some(t0 + Duration::from_secs(1)));
    frame(&mut app, wid);
    bytes(&app, session, "$ clear\r\n\x1b[H\x1b[2J$ cat card.txt\r\n");
    let card = inline_claude(VERSION);
    bytes(
        &app,
        session,
        card.strip_prefix("\r\n").expect("a blank row first"),
    );
    bytes(&app, session, "\r\n");
    resume(&app, session, token);
    set_now(Some(t0 + Duration::from_secs(2)));
    frame(&mut app, wid);
    assert_eq!(
        decorated(&app, wid),
        None,
        "the cat'd card, before the repaint"
    );
    assert_eq!(
        bare_reasons(&app, session).last().map(String::as_str),
        Some("fence")
    );
    let row_of = |row: &str| -> i64 {
        row.split(' ')
            .find_map(|w| w.strip_prefix("row="))
            .and_then(|r| r.parse().ok())
            .expect("a row")
    };
    let fenced = dev_rows(&app, session, "card-bare");
    let catted = row_of(fenced.last().expect("the cat'd card's row"));
    bytes(&app, session, &inline_claude(VERSION));
    set_now(Some(t0 + Duration::from_secs(3)));
    frame(&mut app, wid);
    assert!(decorated(&app, wid).is_some(), "the repaint is decorated");
    let moved = dev_rows(&app, session, "card-row");
    assert_eq!(moved.len(), 1, "{moved:?}");
    assert!(moved[0].contains(&format!(
        "token={}",
        crate::holder_image::token_word(Some(token))
    )));
    assert!(
        row_of(&moved[0]) > catted,
        "under the cat'd card: {moved:?} {fenced:?}"
    );
    assert_eq!(
        dev_rows(&app, session, "card-decorated").len(),
        1,
        "one first frame a drawer"
    );
    assert_eq!(
        dev_rows(&app, session, "card-armed").len(),
        1,
        "nothing armed again"
    );
    set_now(None);
    host.shutdown_and_join();
}

/// A width resize moves fence anchors with reflowed rows so the card stays decorated.
#[test]
fn a_width_resize_keeps_the_card_decorated() {
    let run = |lend: bool| {
        let (mut app, wid, session, host) = bare_window();
        for _ in 0..3 {
            bytes(&app, session, &format!("{}\r\n", "x".repeat(200)));
        }
        bytes(&app, session, "$ claude\r\n");
        launch(&app, session, VERSION, PGID, 1);
        bytes(&app, session, &inline_claude(VERSION));
        frame(&mut app, wid);
        let before = decorated(&app, wid).expect("decorated before");
        let resize = |app: &mut App| assert!(app.apply_term_resize(wid, 24, 220));
        if lend {
            resize(&mut app);
        } else {
            with_knob(FenceKnob::NoReflowLend, || resize(&mut app));
        }
        frame(&mut app, wid);
        let after = decorated(&app, wid);
        let reasons = bare_reasons(&app, session);
        host.shutdown_and_join();
        (before, after, reasons)
    };
    let (before, after, _) = run(true);
    let after = after.expect("decorated after the resize");
    assert!(
        after.top < before.top,
        "the card moved up: {before:?} {after:?}"
    );
    let (_, after, reasons) = run(false);
    assert_eq!(after, None, "CONTROL: no lend, bare: {reasons:?}");
}

/// Claude Code 2.1.304's inline redraw after a width resize at 120 columns, byte for byte (the
/// project path redacted to the same length): inside DEC 2026, home, every row erased whole down
/// the screen, home again, then its card from the top, the composer between its rules and the mode
/// row, the cursor moved back up into the composer.
const CLAUDE_2_1_304_RESIZE_REDRAW: &str =
    include_str!("../tests/fixtures/claude-card/claude-2.1.304-inline-resize-redraw-120col.out");

/// A width resize under 2.1.304's own redraw from home keeps its card decorated: the erase law.
#[test]
fn a_card_2_1_304_redraws_from_home_after_a_resize_is_decorated() {
    const BUILD: &str = "2.1.304";
    let run = || {
        let (mut app, wid, session, host) = bare_window();
        assert!(app.apply_term_resize(wid, 50, 144));
        bytes(
            &app,
            session,
            "$ claude\r\naterm: yolo (default): running 'claude' '--dangerously-skip-permissions'\r\n",
        );
        launch(&app, session, BUILD, PGID, 1);
        bytes(&app, session, CLAUDE_2_1_304_INLINE);
        frame(&mut app, wid);
        let before = decorated(&app, wid);
        assert!(app.apply_term_resize(wid, 50, 120));
        bytes(&app, session, CLAUDE_2_1_304_RESIZE_REDRAW);
        frame(&mut app, wid);
        let after = decorated(&app, wid);
        let reasons = bare_reasons(&app, session);
        let armed = dev_rows(&app, session, "card-armed").len();
        host.shutdown_and_join();
        (before, after, reasons, armed)
    };
    let (before, after, reasons, armed) = run();
    assert_eq!(
        before.map(|p| p.top),
        Some(2),
        "its first frame, under the two lines: {reasons:?}"
    );
    assert_eq!(
        after.map(|p| p.top),
        Some(0),
        "redrawn from the top: {reasons:?}"
    );
    assert_eq!(armed, 1, "the redraw arms nothing again");
    let (_, after, reasons, _) = with_knob(FenceKnob::ErasedRowsStayOut, run);
    assert_eq!(after, None, "CONTROL: the erased rows kept out");
    assert_eq!(reasons.last().map(String::as_str), Some("fence"));
}

/// The re-show bound holds a card stopped and resumed within [`RESHOW_MS`] until the bound expires.
#[test]
fn the_reshow_bound_holds_a_flapping_card_and_shows_it_on_time() {
    let (mut app, wid, session, host) = bare_window();
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    let ms = |t0: Instant, ms: u64| t0 + Duration::from_millis(ms);
    for i in 0..40 {
        bytes(&app, session, &format!("line {i}\r\n"));
    }
    let token = claude_inline(&app, session, PGID, 1);
    let t0 = Instant::now();
    set_now(Some(t0));
    frame(&mut app, wid);
    assert!(
        decorated(&app, wid).is_some(),
        "the first show is never held"
    );
    set_now(Some(ms(t0, 40)));
    suspend(&app, session);
    frame(&mut app, wid);
    assert_eq!(decorated(&app, wid), None, "hidden by its identity");
    set_now(Some(ms(t0, 80)));
    resume(&app, session, token);
    frame(&mut app, wid);
    assert_eq!(decorated(&app, wid), None, "held back by the bound");
    assert_eq!(
        bare_reasons(&app, session).last().map(String::as_str),
        Some("deferred")
    );
    let until = ms(t0, RESHOW_MS);
    assert_eq!(
        app.claude_card_deadline(ms(t0, 80)),
        Some(until),
        "its wake"
    );
    assert_eq!(
        app.claude_cards_due(ms(t0, 200)),
        Vec::<WindowId>::new(),
        "not yet"
    );
    assert_eq!(app.claude_cards_due(until), vec![wid], "due at the bound");
    set_now(Some(until));
    frame(&mut app, wid);
    assert!(
        decorated(&app, wid).is_some(),
        "shown at the bound, no input"
    );
    assert_eq!(dev_rows(&app, session, "card-armed").len(), 1, "unarmed");
    let term = app.pool.get(session).unwrap().term.clone();
    crate::term_lock(&term).scroll_display(3);
    set_now(Some(ms(t0, RESHOW_MS + 20)));
    frame(&mut app, wid);
    assert_eq!(decorated(&app, wid), None, "scrolled back");
    crate::term_lock(&term).scroll_display(-3);
    set_now(Some(ms(t0, RESHOW_MS + 40)));
    frame(&mut app, wid);
    assert!(decorated(&app, wid).is_some(), "a viewport hide is exempt");
    bytes(&app, session, "\r\n");
    leave(&app, session, false);
    set_now(Some(ms(t0, RESHOW_MS + 50)));
    frame(&mut app, wid);
    claude_inline(&app, session, PGID + 1, 2);
    set_now(Some(ms(t0, RESHOW_MS + 60)));
    frame(&mut app, wid);
    assert!(decorated(&app, wid).is_some(), "a new drawer shows at once");
    assert_eq!(dev_rows(&app, session, "card-armed").len(), 2, "and arms");
    set_now(None);
    host.shutdown_and_join();
}

/// The re-show bound spaces shows by at least [`RESHOW_MS`] under a sustained stop/resume flap.
#[test]
fn the_reshow_bound_holds_a_sustained_flap() {
    let (mut app, wid, session, host) = bare_window();
    let token = claude_inline(&app, session, PGID, 1);
    let t0 = Instant::now();
    let mut starts = Vec::new();
    let mut shown = false;
    for step in 0..60u64 {
        let ms = step * 20;
        set_now(Some(t0 + Duration::from_millis(ms)));
        if ms >= 40 && ms % 40 == 0 {
            if ms % 80 == 40 {
                leave(&app, session, true);
            } else {
                resume_quiet(&app, session, token);
            }
        }
        frame(&mut app, wid);
        let now_shown = decorated(&app, wid).is_some();
        if now_shown && !shown {
            starts.push(ms);
        }
        shown = now_shown;
    }
    assert!(starts.len() >= 3, "the card shows again: {starts:?}");
    for pair in starts.windows(2) {
        assert!(
            pair[1] - pair[0] >= RESHOW_MS,
            "shows {pair:?} ms closer than {RESHOW_MS} ms: {starts:?}"
        );
    }
    assert!(
        bare_reasons(&app, session).iter().any(|r| r == "deferred"),
        "held back while flapping"
    );
    assert_eq!(
        dev_rows(&app, session, "card-armed").len(),
        1,
        "never re-armed"
    );
    set_now(None);
    host.shutdown_and_join();
}

/// `fg` with nothing written: the stopped Claude Code (`token`, group
/// [`PGID`]) back in front, its image published again and the fence cut back
/// to it.
fn resume_quiet(app: &App, session: u64, token: u64) {
    {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.note_foreground_group(PGID);
        timeline.offer_holder_image(
            Arc::new(claude_image(PGID, VERSION, 1)),
            crate::holder_image::EdgeKind::Group,
        );
    }
    cut(app, session, Some(token), CutKind::Group, false);
}

/// `card-decorated` dev rows count frames composed between a card's appearance and its decoration.
#[test]
fn the_dev_rows_count_the_frames_a_drawers_card_was_bare() {
    let screen = card_screen(24, 120, MODEL, VERSION);
    let (mut app, wid, session, host) = bare_window();
    launch(&app, session, VERSION, PGID, 1);
    frame(&mut app, wid);
    feed(&app, session, &screen);
    frame(&mut app, wid);
    frame(&mut app, wid);
    let rows = dev_rows(&app, session, "card-decorated");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].contains("bare_frames=0"), "{rows:?}");
    assert_eq!(dev_rows(&app, session, "card-armed").len(), 1);
    assert_eq!(bare_reasons(&app, session), vec!["no-card".to_string()]);
    host.shutdown_and_join();
    let (mut app, wid, session, host) = bare_window();
    paint_at(&app, session, &screen);
    bytes(&app, session, "\x1b[H");
    launch(&app, session, VERSION, PGID, 1);
    frame(&mut app, wid);
    frame(&mut app, wid);
    feed(&app, session, &screen);
    frame(&mut app, wid);
    let rows = dev_rows(&app, session, "card-decorated");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].contains("bare_frames=2"), "CONTROL: {rows:?}");
    host.shutdown_and_join();
    let (mut app, wid, session, host) = bare_window();
    let t0 = Instant::now();
    set_now(Some(t0));
    bytes(&app, session, "$ claude\r\n");
    launch(&app, session, VERSION, PGID, 1);
    bytes(&app, session, &inline_claude(VERSION));
    frame(&mut app, wid);
    bytes(&app, session, "\r\n");
    leave(&app, session, false);
    bytes(&app, session, "$ claude\r\n");
    launch(&app, session, VERSION, PGID + 1, 2);
    set_now(Some(t0 + Duration::from_secs(2)));
    frame(&mut app, wid);
    frame(&mut app, wid);
    bytes(&app, session, &inline_claude(VERSION));
    frame(&mut app, wid);
    let rows = dev_rows(&app, session, "card-decorated");
    assert_eq!(rows.len(), 2, "{rows:?}");
    let row_of = |row: &str| {
        row.split(' ')
            .find_map(|w| w.strip_prefix("row="))
            .map(str::to_string)
            .expect("a row")
    };
    let (old_row, new_row) = (row_of(&rows[0]), row_of(&rows[1]));
    assert_ne!(old_row, new_row, "{rows:?}");
    assert!(
        rows[1].contains("bare_frames=0"),
        "the old card's frames: {rows:?}"
    );
    let fenced: Vec<String> = dev_rows(&app, session, "card-bare")
        .into_iter()
        .filter(|row| row.contains("reason=fence"))
        .collect();
    assert_eq!(fenced.len(), 1, "{fenced:?}");
    assert_eq!(
        row_of(&fenced[0]),
        old_row,
        "the refused card is the old one: {fenced:?}"
    );
    set_now(None);
    host.shutdown_and_join();
}

/// `spawn::adopt_restored_holder` re-derives the foreground holder from the kernel after restore.
#[cfg(unix)]
#[test]
fn a_restore_re_derives_the_holder_from_the_kernel() {
    use aterm_core::terminal::{CutKind, HolderAdoption, HolderCut, Terminal};
    fn in_front(_: i32) -> i32 {
        PGID
    }
    fn nobody(_: i32) -> i32 {
        -1
    }
    fn claude(pgid: i32, _: Option<&HolderImage>) -> HolderImage {
        claude_image(pgid, VERSION, 1)
    }
    fn agy(pgid: i32, _: Option<&HolderImage>) -> HolderImage {
        agy_image(pgid, "1.4.2")
    }
    let token = claude_image(PGID, VERSION, 1).token.expect("a token");
    let nonce = [7u8; 32];
    let id = aterm_core::terminal::ShellIntegrationNonce(nonce).to_hex();
    let banner_row = |t: &Terminal| {
        t.main_grid()
            .visible_to_absolute(t.main_grid().cursor_row())
            + 1
    };
    let mut t = Terminal::new(24, 120);
    t.authorize_shell_integration(nonce);
    t.set_require_shell_integration_nonce(true);
    let card = |t: &mut Terminal| {
        let rows = inline_claude(VERSION);
        t.process(format!("{rows}\r\n").as_bytes());
    };
    t.process(
        format!(
            "\x1b]133;A;id={id}\x07$ \x1b]133;B;id={id}\x07cat card.txt\r\n\x1b]133;C;id={id}\x07"
        )
        .as_bytes(),
    );
    let catted = banner_row(&t);
    card(&mut t);
    t.process(format!("\x1b]133;D;0;id={id}\x07").as_bytes());
    t.process(
        format!("\x1b]133;A;id={id}\x07$ \x1b]133;B;id={id}\x07claude\r\n\x1b]133;C;id={id}\x07")
            .as_bytes(),
    );
    let own = banner_row(&t);
    card(&mut t);
    let rows = |at: u64| (at, at + 3);
    let no_fence = t.checkpoint();
    assert!(no_fence.holder.is_empty(), "a predecessor with no fence");
    let adopt = |c: &aterm_core::terminal::TerminalCheckpoint,
                 probe: fn(i32) -> i32,
                 image: crate::holder_image::ReadImage| {
        let term = std::sync::Mutex::new(Terminal::from_checkpoint(c));
        let timeline = std::sync::Mutex::new(crate::session_timeline::SessionTimeline::default());
        {
            let timeline = timeline.lock().unwrap();
            assert_eq!(timeline.slog().opened_wall(), None, "not registered");
            assert!(
                timeline.slog().log().is_open(aterm_slog::Node::SESSION),
                "not closed does not mean the session has opened"
            );
        }
        let adoption = crate::spawn::adopt_restored_holder(
            &term,
            &timeline,
            &crate::command_ran::RanFeed::blind(std::sync::Arc::default(), SHELL_PGID),
            -1,
            SHELL_PGID,
            probe,
            image,
            |_| Vec::new(),
        );
        let view = term.lock().unwrap().holder_view();
        let timeline = timeline.lock().unwrap();
        let published = timeline.holder_image().and_then(|i| i.token);
        assert_eq!(
            timeline.slog().block_cursor(),
            None,
            "an unopened log leaves every restored block for registration"
        );
        (adoption, view, published, timeline.card_gate())
    };
    let (adoption, view, published, gate) = adopt(&no_fence, in_front, claude);
    assert_eq!(adoption, HolderAdoption::Seeded);
    assert_eq!(published, Some(token), "the image is published");
    assert_eq!(
        gate,
        Some(CardGate {
            token,
            kind: 3,
            rows: 3,
            build: GateBuild::Exact(build(VERSION)),
        }),
        "current on return, before any status sweep"
    );
    let (lo, hi) = rows(own);
    assert!(view.admits_rows(lo, hi), "its own card: {view:?} {own}");
    let (lo, hi) = rows(catted);
    assert!(!view.admits_rows(lo, hi), "not the cat'd one: {view:?}");
    let unadopted = Terminal::from_checkpoint(&no_fence).holder_view();
    assert_eq!(unadopted.token, 0, "CONTROL: {unadopted:?}");
    let (adoption, view, _, _) = adopt(&no_fence, in_front, agy);
    assert_eq!((adoption, view.token), (HolderAdoption::Bare, 0));
    let (adoption, _, published, gate) = adopt(&no_fence, nobody, claude);
    assert_eq!((adoption, published), (HolderAdoption::Bare, None));
    assert_eq!(gate, None, "no current group grants no card gate");
    let mut fenced = Terminal::new(24, 120);
    fenced.process(b"$ claude\r\n");
    fenced.note_holder_cut(HolderCut {
        to: Some(token),
        kind: CutKind::Group,
        from_alive: false,
    });
    let own = banner_row(&fenced);
    card(&mut fenced);
    let (adoption, view, _, gate) = adopt(&fenced.checkpoint(), in_front, claude);
    assert_eq!(adoption, HolderAdoption::Continued);
    assert_eq!(gate.map(|g| g.token), Some(token), "current carried holder");
    let (lo, hi) = rows(own);
    assert!(
        view.admits_rows(lo, hi),
        "the carried mark's card: {view:?}"
    );
    let term = std::sync::Mutex::new(Terminal::from_checkpoint(&no_fence));
    let timeline = std::sync::Mutex::new(crate::session_timeline::SessionTimeline::for_test());
    {
        let mut timeline = timeline.lock().unwrap();
        let at = timeline.slog().now();
        timeline.slog_mut().open_session(None, None, at);
        assert!(
            timeline.slog().opened_wall().is_some(),
            "registered session"
        );
    }
    let ran = std::sync::Arc::new(crate::command_ran::Ran::default());
    crate::spawn::adopt_restored_holder(
        &term,
        &timeline,
        &crate::command_ran::RanFeed::blind(std::sync::Arc::clone(&ran), SHELL_PGID),
        -1,
        SHELL_PGID,
        in_front,
        claude,
        |_| Vec::new(),
    );
    {
        let timeline = timeline.lock().unwrap();
        assert_eq!(timeline.card_gate().map(|g| g.token), Some(token));
        assert!(
            timeline.slog().block_cursor().is_some(),
            "the open log read its blocks"
        );
    }
    // The restored command's program ran before this process heard anything: the in-front image
    // the restore reads is noted, so the next sweep confirms the running block's detail; the
    // shell in front is no command's. NEGATIVE CONTROL (scratch): a restore that notes nothing
    // fails the first assertion.
    let every = Some(crate::command_ran::Window { from: 0, to: None });
    let noted = |ran: &crate::command_ran::Ran| -> Vec<(i32, Option<String>)> {
        ran.within(every)
            .into_iter()
            .map(|e| (e.pgid, e.names.shown().map(str::to_owned)))
            .collect()
    };
    let file = claude_image(PGID, VERSION, 1)
        .exe
        .file_name()
        .map(|n| n.to_string_lossy().into_owned());
    assert!(file.is_some(), "the image has a file");
    assert_eq!(noted(&ran), [(PGID, file)], "the in-front image is noted");
    fn shell_in_front_now(_: i32) -> i32 {
        SHELL_PGID
    }
    let quiet = std::sync::Arc::new(crate::command_ran::Ran::default());
    crate::spawn::adopt_restored_holder(
        &term,
        &timeline,
        &crate::command_ran::RanFeed::blind(std::sync::Arc::clone(&quiet), SHELL_PGID),
        -1,
        SHELL_PGID,
        shell_in_front_now,
        claude,
        |_| Vec::new(),
    );
    assert!(
        noted(&quiet).is_empty(),
        "the shell in front is no command's"
    );
}

/// Card engine §3.7, marks retired by liveness, across a restore: `adopt_restored_holder` books
/// the group the adopted holder leads and, among the shell's children, the group each carried
/// stopped drawer leads, so a stopped drawer that dies after the restore still retires its mark.
#[cfg(unix)]
#[test]
fn a_restore_books_the_groups_its_carried_stopped_drawers_lead() {
    use aterm_core::terminal::{CutKind, HolderCut, Terminal};
    const STOPPED: i32 = PGID + 1;
    fn in_front(_: i32) -> i32 {
        PGID
    }
    fn claude(pgid: i32, _: Option<&HolderImage>) -> HolderImage {
        claude_image(pgid, VERSION, 1)
    }
    fn jobs(_: i32) -> Vec<i32> {
        vec![PGID, STOPPED, SHELL_PGID + 9]
    }
    fn no_jobs(_: i32) -> Vec<i32> {
        Vec::new()
    }
    let front = claude_image(PGID, VERSION, 1).token.expect("a token");
    let stopped = claude_image(STOPPED, VERSION, 1).token.expect("a token");
    let mut t = Terminal::new(24, 120);
    t.process(b"$ claude\r\n");
    t.note_holder_cut(HolderCut {
        to: Some(stopped),
        kind: CutKind::Group,
        from_alive: false,
    });
    t.process(b"its card\r\n");
    t.note_holder_cut(HolderCut {
        to: None,
        kind: CutKind::Group,
        from_alive: true,
    });
    let carried = t.checkpoint();
    let restore = |children: fn(i32) -> Vec<i32>| {
        let term = std::sync::Mutex::new(Terminal::from_checkpoint(&carried));
        assert_eq!(
            term.lock()
                .unwrap()
                .holder_fence()
                .tokens()
                .collect::<Vec<_>>(),
            [stopped],
            "PRECONDITION: the stopped drawer's mark is carried"
        );
        let timeline = std::sync::Mutex::new(crate::session_timeline::SessionTimeline::default());
        crate::spawn::adopt_restored_holder(
            &term,
            &timeline,
            &crate::command_ran::RanFeed::blind(std::sync::Arc::default(), SHELL_PGID),
            -1,
            SHELL_PGID,
            in_front,
            claude,
            children,
        );
        let timeline = timeline.lock().unwrap();
        let groups = timeline.drawer_groups();
        (groups.led_by(PGID), groups.led_by(STOPPED))
    };
    assert_eq!(
        restore(jobs),
        (Some(front), Some(stopped)),
        "the adopted holder's group and the carried stopped drawer's"
    );
    assert_eq!(
        restore(no_jobs),
        (Some(front), None),
        "CONTROL: without the children read, the carried stopped drawer's group is unknown"
    );
}

/// The image of a Claude Code of unknown build (a Homebrew install, a development link) at `exe`,
/// leading group `pgid`, started at `start`: the Registry rung, held to the build its own registry
/// row reports.
fn unknown_claude(pgid: i32, exe: &str, start: u64) -> HolderImage {
    use atpkg::image::{ImageBuild, ProgramTag};
    HolderImage::of(
        pgid,
        Some(start),
        Some(std::path::PathBuf::from(exe)),
        ImageBuild::unknown(ProgramTag::new("claude")),
        1,
    )
}

/// The registry row of conversation `id` reporting `version`, pushed by the
/// footer's resolver for group `pgid` (applied only while it is in front).
fn registry_row(app: &App, session: u64, pgid: i32, id: &str, version: &str) {
    use aterm_agent::harness::footer::{FactsOwner, FooterFacts};
    let entry = app.pool.get(session).expect("session");
    let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    assert!(timeline.set_claude_footer(
        pgid,
        None,
        Some(FooterFacts {
            version: Some(version.into()),
            owner: Some(FactsOwner {
                floor: None,
                session_id: id.into(),
            }),
            ..FooterFacts::default()
        }),
    ));
}

/// `session`'s gate as the splice reads it.
fn gate_of(app: &App, session: u64) -> Option<CardGate> {
    app.pool
        .get(session)
        .expect("session")
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .card_gate()
}

/// Group `pgid` takes the front with `image`, as the reader publishes it on
/// a group edge (`kind`), and the fence is cut to it.
fn take_front(app: &App, session: u64, pgid: i32, image: HolderImage, kind: CutKind) -> u64 {
    let token = image.token.expect("a token");
    {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.note_foreground_group(pgid);
        let edge = match kind {
            CutKind::Exec => crate::holder_image::EdgeKind::Exec,
            _ => crate::holder_image::EdgeKind::Group,
        };
        timeline.offer_holder_image(Arc::new(image), edge);
    }
    cut(app, session, Some(token), kind, false);
    token
}

/// Ctrl-Z as the reader sees it: the shell's group and image in front, the stopped program's
/// footer facts left standing for its group (nothing drops them on a group edge), the fence cut to
/// no drawer.
fn shell_in_front(app: &App, session: u64) {
    use atpkg::image::{ImageBuild, ProgramTag};
    {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.note_foreground_group(SHELL_PGID);
        timeline.offer_holder_image(
            Arc::new(HolderImage::of(
                SHELL_PGID,
                Some(1),
                Some(std::path::PathBuf::from("/bin/zsh")),
                ImageBuild::unknown(ProgramTag::new("zsh")),
                1,
            )),
            crate::holder_image::EdgeKind::Group,
        );
    }
    cut(app, session, None, CutKind::Group, true);
}

/// The registry rung decorates an unknown-build Claude Code once its row lands, across Ctrl-Z and
/// `fg`.
#[test]
fn the_registry_rung_survives_ctrl_z_and_fg() {
    const BREW: &str = "/opt/homebrew/bin/claude";
    let run = |between: bool| {
        let (mut app, wid, session, host) = bare_window();
        let t0 = Instant::now();
        set_now(Some(t0));
        bytes(&app, session, "$ claude\r\n");
        let token = take_front(
            &app,
            session,
            PGID,
            unknown_claude(PGID, BREW, 1),
            CutKind::Group,
        );
        bytes(&app, session, &inline_claude(VERSION));
        frame(&mut app, wid);
        assert_eq!(decorated(&app, wid), None, "bare until its row lands");
        assert_eq!(
            bare_reasons(&app, session).last().map(String::as_str),
            Some("build")
        );
        registry_row(&app, session, PGID, "sess-a", VERSION);
        set_now(Some(t0 + Duration::from_millis(50)));
        frame(&mut app, wid);
        assert!(decorated(&app, wid).is_some(), "its own row: decorated");
        bytes(&app, session, "\r\n");
        shell_in_front(&app, session);
        bytes(&app, session, "zsh: suspended  claude\r\n");
        set_now(Some(t0 + Duration::from_secs(1)));
        frame(&mut app, wid);
        assert_eq!(decorated(&app, wid), None, "bare while stopped");
        if between {
            bytes(&app, session, "$ agy\r\n");
            take_front(
                &app,
                session,
                PGID + 7,
                agy_image(PGID + 7, "1.4.2"),
                CutKind::Group,
            );
            bytes(&app, session, "bye\r\n");
            shell_in_front(&app, session);
        }
        bytes(&app, session, "$ fg\r\n");
        take_front(
            &app,
            session,
            PGID,
            unknown_claude(PGID, BREW, 1),
            CutKind::Group,
        );
        bytes(&app, session, &inline_claude(VERSION));
        set_now(Some(t0 + Duration::from_secs(2)));
        frame(&mut app, wid);
        let gate = gate_of(&app, session);
        let place = decorated(&app, wid);
        let reasons = bare_reasons(&app, session);
        let armed = dev_rows(&app, session, "card-armed").len();
        set_now(None);
        host.shutdown_and_join();
        (token, gate, place, reasons, armed)
    };
    for between in [false, true] {
        let (token, gate, place, reasons, armed) = run(between);
        assert_eq!(
            gate,
            Some(CardGate {
                token,
                kind: 3,
                rows: 3,
                build: GateBuild::Registry(build(VERSION)),
            }),
            "between={between}: held to its own row after fg"
        );
        assert!(
            place.is_some(),
            "between={between}: decorated again: {reasons:?}"
        );
        assert_eq!(armed, 1, "between={between}: the same episode");
    }
}

/// The registry rung holds an unknown-build image to its own row alone, never another group's or a
/// replaced image's.
#[test]
fn the_registry_rung_holds_an_image_to_its_own_row_alone() {
    let (mut app, wid, session, host) = bare_window();
    take_front(
        &app,
        session,
        PGID + 1,
        unknown_claude(PGID + 1, "/opt/homebrew/bin/claude", 1),
        CutKind::Group,
    );
    registry_row(&app, session, PGID + 1, "other", VERSION);
    let first = take_front(
        &app,
        session,
        PGID,
        unknown_claude(PGID, "/opt/homebrew/bin/claude", 2),
        CutKind::Group,
    );
    assert_eq!(
        gate_of(&app, session).map(|g| (g.token, g.build)),
        Some((first, GateBuild::None)),
        "another group's row is not this image's"
    );
    registry_row(&app, session, PGID, "sess-a", "2.1.282");
    assert_eq!(
        gate_of(&app, session).map(|g| g.build),
        Some(GateBuild::Registry(build("2.1.282"))),
        "CONTROL: its own row"
    );
    let second = take_front(
        &app,
        session,
        PGID,
        unknown_claude(PGID, "/Users//dev/src/claude/dist/claude", 2),
        CutKind::Exec,
    );
    assert_ne!(first, second, "a new image");
    feed(&app, session, &card_screen(24, 120, MODEL, VERSION));
    frame(&mut app, wid);
    assert_eq!(
        gate_of(&app, session).map(|g| g.build),
        Some(GateBuild::None),
        "the replaced image's row is not the new image's"
    );
    assert_eq!(decorated(&app, wid), None, "bare until its own row");
    registry_row(&app, session, PGID, "sess-b", VERSION);
    frame(&mut app, wid);
    assert_eq!(
        gate_of(&app, session).map(|g| g.build),
        Some(GateBuild::Registry(build(VERSION)))
    );
    assert!(decorated(&app, wid).is_some(), "its own row: decorated");
    host.shutdown_and_join();
    let (mut app, wid, session, host) = bare_window();
    {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.note_foreground_group(PGID);
    }
    registry_row(&app, session, PGID, "sess-c", VERSION);
    take_front(
        &app,
        session,
        PGID,
        unknown_claude(PGID, "/opt/homebrew/bin/claude", 3),
        CutKind::Group,
    );
    feed(&app, session, &card_screen(24, 120, MODEL, VERSION));
    frame(&mut app, wid);
    assert_eq!(
        gate_of(&app, session).map(|g| g.build),
        Some(GateBuild::Registry(build(VERSION))),
        "the row the group arrived with"
    );
    assert!(decorated(&app, wid).is_some(), "decorated");
    let next = take_front(
        &app,
        session,
        PGID,
        unknown_claude(PGID, "/Users//dev/src/claude/dist/claude", 3),
        CutKind::Exec,
    );
    assert_eq!(
        gate_of(&app, session).map(|g| g.build),
        Some(GateBuild::None),
        "CONTROL: the replaced image's row"
    );
    shell_in_front(&app, session);
    take_front(
        &app,
        session,
        PGID,
        unknown_claude(PGID, "/Users//dev/src/claude/dist/claude", 3),
        CutKind::Group,
    );
    assert_eq!(
        gate_of(&app, session).map(|g| (g.token, g.build)),
        Some((next, GateBuild::None)),
        "still the replaced image's after fg"
    );
    host.shutdown_and_join();
}
