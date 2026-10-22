// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE GATE SWITCH (card engine §3.8-§3.9, §7 "A, the gate"): the card is
//! decorated only where the current drawer's token, its reader, the frame's
//! fence and the banner's build policy all admit it — on a real engine, a
//! real frame through the capture route's splices and the real splice — and
//! a card hidden by its identity or its fence shows again no sooner than the
//! re-show bound allows.

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

/// Claude Code's launch card and prompt as it paints them INLINE (no clear),
/// from the cursor down: a blank row, the card, a blank row, the composer
/// between its rules and the mode row.
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

/// `rows` written at their own rows from the top with NO clear (cursor
/// addressing and erase-line alone, the path row in [`GRAY`] as [`feed`]
/// paints it): what a program that does not clear the screen leaves.
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

/// THE GATE, AS A FUNCTION (§3.8): integer checks, cheapest first, each one
/// failing closed — the token the frame's fence names, the reader, the
/// fence (one span holding every card row on main; the alternate screen
/// wholly the drawer's on alt) and the build policy (Exact: the banner names
/// the image's build; Registry: its own row's; ProvenanceOnly: the fence
/// alone; None: bare). Each refusal is shown against the admitted base case
/// it differs from by one field.
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
    };
    let all = view(&[(100, u64::MAX)], false, false);
    assert_eq!(
        admit(&gate, &main(all), &read),
        Ok(()),
        "the drawer's own card"
    );
    // The token.
    let no_token = CardGate { token: 0, ..gate };
    assert_eq!(admit(&no_token, &main(all), &read), Err(Bare::Token));
    let other = HolderView {
        token: T + 1,
        ..all
    };
    assert_eq!(admit(&gate, &main(other), &read), Err(Bare::Token));
    // The reader: an agy card under a Claude Code drawer.
    let agy = CardGate { kind: 4, ..gate };
    assert_eq!(admit(&agy, &main(all), &read), Err(Bare::Kind));
    // The fence, main.
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
    // The fence, alt: the screen's freshness alone, whatever main holds.
    let alt = |holder| FenceFrame {
        holder,
        base_y: 100,
        alt: true,
    };
    assert_eq!(admit(&gate, &alt(all), &read), Err(Bare::Fence));
    assert_eq!(
        admit(&gate, &alt(view(&[], true, true)), &read),
        Ok(()),
        "a fresh alternate screen is wholly the drawer's"
    );
    // The build policy.
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

/// THE POLICY TABLE (§3.8): the image's source × its program's banner
/// build, as `aterm_phase` declares it beside each reader. Claude Code's
/// managed-store, ALab-record and vendor-installer images hold the banner to
/// their own build; an image of unknown build holds it to its own registry
/// row, or keeps it bare with none; agy and Muse Code are judged by the
/// fence alone whatever their source; a shell or a launcher has no gate. The
/// timeline gives the gate only for its CURRENT image (`pgid ==
/// program_pgid`) — CONTROL: the same image with another group in front has
/// none.
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
    // The timeline's gate: the current image's only.
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

/// THE FIRST FRAME (§3.8, §8): a Claude Code whose image is published and
/// whose fence is cut is decorated on the first frame that shows its card,
/// with no footer facts — no registry row is waited on — for ONE timeline
/// lock a pane a frame; settled (held, its window out of focus: a focused
/// card flows on), it costs no clock read and no allocation.
/// The dev card probe (on here, as in every §8 run) takes one lock more a
/// pane, to record the pass, and none with the probe off. CONTROL: the same
/// image with another group in front (not current) draws nothing, for the
/// same one lock.
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
    // Settled: held, its window out of focus (a focused card flows on,
    // `settled_glass`).
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
    // CONTROL: the image no longer current.
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

/// THE GATE'S CONTROLS ON THE GLASS (§7 "A, the gate"): the very screen that
/// decorates under its own drawer stays bare, for the reason the probe's
/// `card-bare` row names, under each of: a 2.1.303 image (Exact), a fence
/// that names another token, an agy card (rows kind 4) under Claude Code, a
/// mark tainted by what lay under the cursor at its cut, and a card the
/// drawer did not write (above its span). Each CONTROL decorates the same
/// card once the one thing it differs by is the drawer's.
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
    // A 2.1.303 image under a 2.1.283 banner.
    let (place, reasons) = run(&|app, session| {
        launch(app, session, "2.1.303", PGID, 1);
        feed(app, session, &screen);
    });
    assert_eq!(
        (place, reasons.last().map(String::as_str)),
        (None, Some("build"))
    );
    // The frame's fence names another drawer.
    let (place, reasons) = run(&|app, session| {
        let token = launch(app, session, VERSION, PGID, 1);
        cut(app, session, Some(token ^ 1), CutKind::Exec, false);
        feed(app, session, &screen);
    });
    assert_eq!(
        (place, reasons.last().map(String::as_str)),
        (None, Some("token"))
    );
    // An agy card under Claude Code.
    let agy = agy_card_screen("1.4.0");
    let (place, reasons) = run(&|app, session| {
        launch(app, session, VERSION, PGID, 1);
        feed(app, session, &agy);
    });
    assert_eq!(
        (place, reasons.last().map(String::as_str)),
        (None, Some("kind"))
    );
    // Tainted: the card was on the screen BELOW the cursor at the cut, and
    // nothing cleared it since.
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
    // CONTROL: the lay that finds no ink admits it (the span from the
    // cursor down holds the card).
    let (place, _) = with_knob(FenceKnob::NoLayTaint, || run(&tainted));
    assert!(
        place.is_some(),
        "CONTROL: untainted, the same card decorates"
    );
    // Outside the span: printed by `cat` before the drawer took the front.
    let (place, reasons) = run(&|app, session| {
        cat_card(app, session);
        launch(app, session, VERSION, PGID, 1);
    });
    assert_eq!(
        (place, reasons.last().map(String::as_str)),
        (None, Some("fence"))
    );
    // CONTROL: the same card printed after the cut is the drawer's.
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

/// The image of an agy of build `version` leading group `pgid`: a store
/// image — whose banner the gate does not hold to its build.
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

/// AGY IS JUDGED BY ITS FENCE ALONE (§3.8, ProvenanceOnly): an agy 1.4.2
/// store image decorates the card it paints whatever build its banner
/// prints (every 1.4.x fixture prints 1.4.0), and a predecessor's — or a
/// quoted — agy card above its span stays bare, for the fence's reason only.
/// CONTROL: the self-referential compare A4 deleted (the card's banner held
/// to the build read off that same banner, with no fence) passes that card;
/// the build policy here passes it too — the fence is what keeps it bare.
/// The fence holds agy's card to its reader's full height
/// (`agy::LAUNCH_CARD_ROWS`, the feet row under the four rows it places):
/// a span that ends above the feet is no span of the card's.
/// Mutation-checked: the fence's height taken from the placed rows alone
/// admits the card without its feet.
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
        // What the build policy alone says of the card on the glass.
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
    // The fence's height: the reader's, from the banner to the feet.
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

/// A SAME-BUILD INLINE RELAUNCH (§7; `/exit`, then `claude` again): the old
/// card, the same build's, is still on the glass above the new process's
/// span; the new process is a new snapshot token, and until it paints its
/// own card the old one stays bare (the fence) and arms nothing. Its own
/// card is decorated and armed: a new episode. Mutation-checked: a span that
/// starts at row 0 (`span.start = 0`) decorates the old card.
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
    // `/exit`: the shell is back; `claude` again, the same build.
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
    // The new process paints its own card under it.
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

/// Claude Code 2.1.304's first inline frame, byte for byte as it wrote it in
/// the lab at 144 columns (`CLAUDE_CODE_NO_FLICKER=0`, 2026-10-20): its
/// card, the composer between its rules and the mode row, one write of 871
/// characters, ending with the cursor moved back up into the composer.
const CLAUDE_2_1_304_INLINE: &str =
    include_str!("../tests/fixtures/claude-card/claude-2.1.304-inline-first-frame-144col.out");

/// THE SAME-BUILD INLINE RELAUNCH ON 2.1.304's OWN BYTES (§7: the fixture
/// re-captured for 2.1.304): its first frame, `/exit` (the shell's prompt
/// under its frame), and the same build again, its first frame drawn under
/// the old one. Until then the old card stays bare (its rows are outside the
/// new drawer's span); the new card is decorated on its first frame and
/// armed: a new episode. Mutation-checked with
/// [`a_same_build_inline_relaunch_keeps_the_old_card_bare`]: a span opened
/// at row 0 (`open_span(mark, 0, grid)` in `lay`) decorates the old card.
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
    // `/exit`: under the mode row, the shell's prompt; `claude` again.
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

/// THE FULLSCREEN GAP (§7): Claude Code under `CLAUDE_CODE_NO_FLICKER=1`
/// takes the alternate screen. Before it does, a card on the main screen
/// outside its span stays bare; after its `1049h` (the alternate screen
/// cleared) the card it paints there is decorated. CONTROL: a `47h` (the
/// alternate screen NOT cleared) leaves the same card bare: nothing says the
/// alternate screen is wholly the drawer's.
#[test]
fn the_alternate_screen_is_the_drawers_only_once_cleared() {
    let screen = card_screen(24, 120, MODEL, VERSION);
    let run = |enter: &str| {
        let (mut app, wid, session, host) = bare_window();
        // A card on main from before the launch.
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
/// inline, under the command line. The drawer's token.
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

/// CTRL-Z, `clear`, `cat card`, `fg` (§7): the card the shell printed while
/// Claude Code was stopped — the same build's, byte for byte, under the
/// shell's command line — stays bare when Claude is back in front: the
/// stopped drawer's sealed rows were rewritten. Nothing is armed again.
/// CONTROLS: without the seal check the cat'd card is decorated (the seal
/// is what refuses it), and still nothing arms (the same drawer, the same
/// episode); and `fg` with nothing rewritten shows Claude's own card
/// again, unarmed. (The seal judges a row by its content: a rewrite that
/// leaves every sealed row exactly as the drawer left it is no rewrite.)
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
            // `clear`, then `cat card.txt`: the card's own rows, from its
            // banner.
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

/// CTRL-Z, `clear`, `cat card`, `fg` AS CLAUDE CODE 2.1.304 PLAYS IT (§8,
/// live: on `SIGCONT` it repaints its card under the shell's `fg`, 2-3 ms
/// after the cut). Until the repaint the cat'd card is the one on the pane
/// and stays bare (its fence); the repaint is the drawer's own, drawn after
/// its cut, and is decorated where it is drawn, under the cat'd card. The
/// dev rows say the decorated card moved (`card-row`), and nothing is armed
/// again: the same episode. Mutation-checked: arming every re-show of the
/// same drawer's card (`entry.arm` when `!entry.was_shown`) arms twice; a
/// `card-row` never recorded fails its row.
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
    // Claude Code's repaint, at its cursor under `fg`.
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

/// A WIDTH RESIZE (§7): rows above Claude Code's span that unwrap at a wider
/// width move its card up, and the fence's anchors move with them (the grid
/// lends them to the reflow): the card stays decorated at its new place.
/// CONTROL: a reflow that lends nothing leaves the anchors at their old rows
/// and the card bare.
#[test]
fn a_width_resize_keeps_the_card_decorated() {
    let run = |lend: bool| {
        let (mut app, wid, session, host) = bare_window();
        // Three shell lines that wrap at 120 columns, then the launch.
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

/// THE RE-SHOW BOUND (§3.9): Claude Code stopped and brought back within
/// [`RESHOW_MS`] of its card's show is not shown again until that bound —
/// the card's own wake asks for the frame at that instant, with no input —
/// then shown, unarmed. A viewport hide (scrolled back and down again) is
/// exempt: shown at once. A NEW drawer's card is never held back, however
/// soon. Mutation-checked: a hide that never bounds the re-show shows the
/// card at once.
#[test]
fn the_reshow_bound_holds_a_flapping_card_and_shows_it_on_time() {
    let (mut app, wid, session, host) = bare_window();
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    let ms = |t0: Instant, ms: u64| t0 + Duration::from_millis(ms);
    // History to scroll back into, then Claude Code.
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
    // Ctrl-Z at 40 ms, `fg` at 80 ms: an identity hide, then a show.
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
    // A viewport hide: scrolled back and down again 20 ms later.
    let term = app.pool.get(session).unwrap().term.clone();
    crate::term_lock(&term).scroll_display(3);
    set_now(Some(ms(t0, RESHOW_MS + 20)));
    frame(&mut app, wid);
    assert_eq!(decorated(&app, wid), None, "scrolled back");
    crate::term_lock(&term).scroll_display(-3);
    set_now(Some(ms(t0, RESHOW_MS + 40)));
    frame(&mut app, wid);
    assert!(decorated(&app, wid).is_some(), "a viewport hide is exempt");
    // `/exit`, then `claude` again 20 ms later: a new drawer, whose first
    // show is never held back.
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

/// THE BOUND HOLDS A SUSTAINED FLAP (§3.9, photosensitivity): Claude Code
/// stopped and resumed every 40 ms for over a second — a hide while a show
/// is held back among them — on a frame every 20 ms. Every show of its card
/// begins at least [`RESHOW_MS`] after the one before (at most
/// `1000 / RESHOW_MS` a second, under WCAG 2.3.1's three; the const assert
/// holds the constant to that), the card does show again (the bound defers,
/// it does not hide), and nothing is armed again. Mutation-checked: a hide
/// that never bounds the re-show starts a show every 80 ms here.
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
        // From 40 ms: stopped at 40, 120, 200, …; resumed at 80, 160, ….
        // No bytes: the shell writes nothing over the card meanwhile.
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

/// THE DEV ROWS' JUDGE (§8): `card-decorated bare_frames=` counts the frames
/// composed after the drawer's card was on the glass and before it was
/// decorated. A fresh launch's card is decorated on its first frame
/// (`bare_frames=0`), and its episode is armed once. CONTROL: a card the
/// gate refuses for two frames (tainted at the cut) and then admits (the
/// drawer clears and repaints) says `bare_frames=2`.
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
    // CONTROL.
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
    // A PREDECESSOR'S CARD (an inline relaunch): refused above the drawer's
    // own for two frames, it is no bare time of the drawer's card, and the
    // `card-bare` row names its row. Mutation-checked: `BareFrames::of`
    // counting a refused card at any row reports `bare_frames=2` here.
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

/// THE HOLDER AFTER A RESTORE (§3.8, `spawn::adopt_restored_holder`): the
/// foreground leader is read from the kernel and its image published. A
/// carried mark that is the holder's continues; a predecessor with no fence
/// is seeded, for an Exact drawer, from its signed output start — the card
/// it drew is admitted, the same-build card `cat` printed above it is not;
/// a ProvenanceOnly drawer, or no group in front, is bare. CONTROL: the
/// restored engine with no adoption admits nothing.
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
    // A signed shell: `cat card.txt` prints a card, then `claude` paints its
    // own. A card's three rows start a row under the cursor it starts at.
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
        let adoption =
            crate::spawn::adopt_restored_holder(&term, &timeline, -1, SHELL_PGID, probe, image);
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
    // CONTROL: no adoption, nothing admitted.
    let unadopted = Terminal::from_checkpoint(&no_fence).holder_view();
    assert_eq!(unadopted.token, 0, "CONTROL: {unadopted:?}");
    // A ProvenanceOnly drawer, and nobody in front: bare.
    let (adoption, view, _, _) = adopt(&no_fence, in_front, agy);
    assert_eq!((adoption, view.token), (HolderAdoption::Bare, 0));
    let (adoption, _, published, gate) = adopt(&no_fence, nobody, claude);
    assert_eq!((adoption, published), (HolderAdoption::Bare, None));
    assert_eq!(gate, None, "no current group grants no card gate");
    // A carried mark of the holder's continues: its predecessor ran the
    // fence, Claude Code in front and its card under its cut.
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
    // An already-open carried log takes the normal block-before-foreground
    // step, while the gate is current on return here too.
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
    crate::spawn::adopt_restored_holder(&term, &timeline, -1, SHELL_PGID, in_front, claude);
    let timeline = timeline.lock().unwrap();
    assert_eq!(timeline.card_gate().map(|g| g.token), Some(token));
    assert!(
        timeline.slog().block_cursor().is_some(),
        "the open log read its blocks"
    );
}

/// The image of a Claude Code of UNKNOWN build (a Homebrew install, a
/// development link) at `exe`, leading group `pgid`, started at `start`:
/// the Registry rung, held to the build its own registry row reports.
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

/// Ctrl-Z as the reader sees it: the shell's group and image in front, the
/// stopped program's footer facts LEFT STANDING for its group (nothing
/// drops them on a group edge), the fence cut to no drawer.
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

/// THE REGISTRY RUNG ACROSS CTRL-Z AND `fg` (§3.8, §3.13): a Claude Code of
/// unknown build is bare until its own registry row lands, then decorated;
/// stopped with Ctrl-Z (its facts left standing for its group) and resumed,
/// its image published again on `fg`'s group edge, it repaints its card —
/// decorated again, held to the same row, unarmed. So too when another
/// drawer with no facts of its own (an agy run while Claude Code was
/// stopped) took the front in between. Mutation-checked: re-recording the
/// stale owner at every publication of a drawer image refuses the image's
/// own row after `fg` (`build`), for good.
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
        // Ctrl-Z.
        bytes(&app, session, "\r\n");
        shell_in_front(&app, session);
        bytes(&app, session, "zsh: suspended  claude\r\n");
        set_now(Some(t0 + Duration::from_secs(1)));
        frame(&mut app, wid);
        assert_eq!(decorated(&app, wid), None, "bare while stopped");
        if between {
            // agy in the shell, its facts never pushed, and gone again.
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
        // `fg`: the same image again, and Claude Code repaints its card.
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

/// THE REGISTRY RUNG HOLDS AN IMAGE TO ITS OWN ROW ALONE (§3.8): an image of
/// unknown build is never held to a row of another group's that the facts
/// slot still keeps, nor to the row that stood for its own group when it
/// was published (the image its `exec` replaced wrote that one); only its
/// own row, from a new owner, decides its build. A row that came for the
/// group before any image of it was published (the byte arm reads the
/// group at its first byte, which a fullscreen Claude Code can write after
/// its row) is the image's that arrives with the group — and stays that
/// image's: the image an `exec` puts in its place, stopped and resumed,
/// is not held to it. Mutation-checked: without the group check the other
/// group's row is taken; without the stale owner the replaced image's row
/// is; a row from before any image counted stale at a group edge keeps the
/// arriving image bare; a row judged an image's own and not made its own
/// is taken by its successor after `fg`.
#[test]
fn the_registry_rung_holds_an_image_to_its_own_row_alone() {
    let (mut app, wid, session, host) = bare_window();
    // Another group's row, still in the facts slot when this one takes
    // the front.
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
    // Its own row; then it execs a development build in place: the row
    // that stood is the replaced image's.
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
    // The new image's own row.
    registry_row(&app, session, PGID, "sess-b", VERSION);
    frame(&mut app, wid);
    assert_eq!(
        gate_of(&app, session).map(|g| g.build),
        Some(GateBuild::Registry(build(VERSION)))
    );
    assert!(decorated(&app, wid).is_some(), "its own row: decorated");
    host.shutdown_and_join();
    // A row before any image of the group: the arriving image's own.
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
    // Its `exec` in place: that row is the replaced image's, after `fg` too.
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
